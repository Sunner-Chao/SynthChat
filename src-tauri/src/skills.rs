use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, AgentDefinition, EnhancedSkillSummary, MarketplaceSkill,
        SkillAuditFinding, SkillAuditReport, SkillBundle, SkillCuratorArchiveCandidate,
        SkillCuratorArchiveRecord, SkillCuratorOverlap, SkillCuratorReport, SkillCuratorState,
        SkillInstallRecord, SkillPromptBlock, SkillTap, SkillUpdateCheck,
    },
    store::AppStore,
};

const MAX_SKILL_PROMPT_CHARS: usize = 16_000;
const MAX_AUDIT_FILES: usize = 64;
const MAX_AUDIT_FILE_BYTES: u64 = 256 * 1024;

pub fn list_skills(store: &AppStore) -> AppResult<Vec<EnhancedSkillSummary>> {
    let existing = store.skills()?;
    if existing.is_empty() || bundled_skill_catalog_needs_refresh(&existing) {
        install_builtin_skills(store)
    } else {
        Ok(existing)
    }
}

pub fn list_skills_for_agent(
    store: &AppStore,
    agent_id: &str,
) -> AppResult<Vec<EnhancedSkillSummary>> {
    let agent = store.agent(Some(agent_id))?;
    let skills = list_skills(store)?
        .into_iter()
        .map(|mut skill| {
            skill.enabled = agent.skills_enabled && agent.enabled_skills.contains(&skill.id);
            skill.agent_id = agent.id.clone();
            skill
        })
        .collect();
    Ok(skills)
}

pub fn install_builtin_skills(store: &AppStore) -> AppResult<Vec<EnhancedSkillSummary>> {
    let persisted = store.skills()?;
    let mut merged = Vec::new();
    let mut seen = HashSet::new();
    for root in discover_skill_roots(store) {
        for path in find_skill_files(&root.path) {
            if let Some(skill) = summarize_skill(&root.path, &path, &root.source) {
                if seen.insert(skill.id.clone()) {
                    merged.push(skill);
                }
            }
        }
    }
    for skill in persisted.into_iter().filter(|skill| !skill.is_bundled) {
        if seen.insert(skill.id.clone()) {
            merged.push(skill);
        }
    }
    merged.sort_by(|a, b| a.id.cmp(&b.id));
    store.set_skills(merged)
}

pub fn list_skill_bundles(store: &AppStore) -> AppResult<Vec<SkillBundle>> {
    let skills = list_skills(store)?;
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for skill in skills {
        let group = skill
            .id
            .split('/')
            .next()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("local")
            .to_string();
        groups.entry(group).or_default().push(skill.id);
    }
    Ok(groups
        .into_iter()
        .map(|(id, skill_ids)| SkillBundle {
            name: title_case(&id.replace('-', " ")),
            description: format!("{} skills from the {id} collection.", skill_ids.len()),
            id,
            skill_ids,
        })
        .collect())
}

pub fn install_skill_bundle(
    store: &AppStore,
    bundle_id: &str,
    agent_id: Option<&str>,
) -> AppResult<Vec<EnhancedSkillSummary>> {
    let bundles = list_skill_bundles(store)?;
    let Some(bundle) = bundles.into_iter().find(|bundle| bundle.id == bundle_id) else {
        return list_skills(store);
    };
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        store.enable_agent_skills(agent_id, bundle.skill_ids)?;
        list_skills_for_agent(store, agent_id)
    } else {
        list_skills(store)
    }
}

pub fn list_marketplace_skills(
    store: &AppStore,
    query: Option<&str>,
) -> AppResult<Vec<MarketplaceSkill>> {
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let mut skills = list_skills(store)?
        .into_iter()
        .filter(|skill| {
            query.is_none_or(|query| {
                let query = query.to_lowercase();
                [
                    skill.id.as_str(),
                    skill.name.as_str(),
                    skill.description.as_str(),
                    skill.source.as_str(),
                    skill.author.as_str(),
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
            })
        })
        .map(marketplace_skill_from_summary)
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(skills)
}

pub fn install_marketplace_skill(
    store: &AppStore,
    skill_id: &str,
    agent_id: Option<&str>,
) -> AppResult<Option<EnhancedSkillSummary>> {
    let skills = list_skills(store)?;
    let Some(resolved_id) = resolve_skill_selector(&skills, skill_id)? else {
        return Ok(None);
    };
    let Some(skill) = skills.iter().find(|skill| skill.id == resolved_id).cloned() else {
        return Ok(None);
    };
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        store.enable_agent_skills(agent_id, vec![skill.id.clone()])?;
        return Ok(list_skills_for_agent(store, agent_id)?
            .into_iter()
            .find(|candidate| candidate.id == skill.id));
    }
    Ok(Some(skill))
}

pub fn audit_skills(store: &AppStore, selector: Option<&str>) -> AppResult<Vec<SkillAuditReport>> {
    let skills = list_skills(store)?;
    let targets = if let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty())
    {
        let Some(resolved_id) = resolve_skill_selector(&skills, selector)? else {
            return Err(AppError::NotFound(format!("skill {selector}")));
        };
        skills
            .into_iter()
            .filter(|skill| skill.id == resolved_id)
            .collect::<Vec<_>>()
    } else {
        skills
    };
    Ok(targets.into_iter().map(audit_skill).collect())
}

pub fn curate_skills_report(store: &AppStore) -> AppResult<SkillCuratorReport> {
    let skills = list_skills(store)?;
    let reports = skills.iter().cloned().map(audit_skill).collect::<Vec<_>>();
    let audit_attention = reports
        .iter()
        .filter(|report| report.status == "attention")
        .count();
    let external_skills = skills.iter().filter(|skill| !skill.is_bundled).count();
    let bundled_skills = skills.iter().filter(|skill| skill.is_bundled).count();
    let overlap_clusters = detect_skill_overlap_clusters(&skills);
    let state = read_skill_curator_state(store)?;
    let usage = read_skill_usage_records(store)?;
    let archive_candidates =
        detect_skill_archive_candidates(&skills, &overlap_clusters, &state, &usage);
    let mut recommendations = Vec::new();
    if !overlap_clusters.is_empty() {
        recommendations.push(format!(
            "Review {} overlap cluster(s) and consolidate sibling skills into class-level umbrellas.",
            overlap_clusters.len()
        ));
    }
    if !archive_candidates.is_empty() {
        recommendations.push(format!(
            "Review {} recoverable archive candidate(s); archive only after confirming their content is redundant or stale.",
            archive_candidates.len()
        ));
    }
    if audit_attention > 0 {
        recommendations.push(format!(
            "Fix {} skill audit report(s) with high/critical findings before enabling broad reuse.",
            audit_attention
        ));
    }
    if recommendations.is_empty() {
        recommendations
            .push("No curator action is recommended from the dry-run heuristics.".into());
    }

    let generated_at = now_iso();
    let report_dir = store.data_dir().join("skills").join("curator");
    fs::create_dir_all(&report_dir)?;
    let report_path = report_dir.join("REPORT.md");
    let mut report = SkillCuratorReport {
        generated_at,
        report_path: report_path.to_string_lossy().to_string(),
        total_skills: skills.len(),
        external_skills,
        bundled_skills,
        audit_attention,
        overlap_clusters,
        archive_candidates,
        recommendations,
    };
    fs::write(&report_path, format_skill_curator_markdown(&report))?;
    report.report_path = report_path.to_string_lossy().to_string();
    let mut state = state;
    state.last_run_at = Some(report.generated_at.clone());
    state.last_report_path = Some(report.report_path.clone());
    state.run_count += 1;
    write_skill_curator_state(store, state)?;
    Ok(report)
}

pub fn maybe_curate_skills_report(
    store: &AppStore,
    interval_hours: usize,
) -> AppResult<Option<SkillCuratorReport>> {
    let mut state = read_skill_curator_state(store)?;
    if state.paused {
        return Ok(None);
    }
    let now = chrono::Utc::now();
    let Some(last_run_at) = state.last_run_at.as_deref() else {
        state.last_run_at = Some(now.to_rfc3339());
        write_skill_curator_state(store, state)?;
        return Ok(None);
    };
    let last_run_at = chrono::DateTime::parse_from_rfc3339(last_run_at)
        .map(|value| value.with_timezone(&chrono::Utc))
        .unwrap_or(now);
    let interval = chrono::Duration::hours(interval_hours.max(1) as i64);
    if now.signed_duration_since(last_run_at) < interval {
        return Ok(None);
    }
    curate_skills_report(store).map(Some)
}

pub fn skill_curator_state(store: &AppStore) -> AppResult<SkillCuratorState> {
    read_skill_curator_state(store)
}

pub fn set_skill_curator_paused(store: &AppStore, paused: bool) -> AppResult<SkillCuratorState> {
    let mut state = read_skill_curator_state(store)?;
    state.paused = paused;
    write_skill_curator_state(store, state)
}

pub fn pin_skill_for_curator(store: &AppStore, selector: &str) -> AppResult<SkillCuratorState> {
    let skills = list_skills(store)?;
    let Some(skill_id) = resolve_skill_selector(&skills, selector)? else {
        return Err(AppError::NotFound(format!("skill {selector}")));
    };
    let mut state = read_skill_curator_state(store)?;
    if !state.pinned_skill_ids.iter().any(|id| id == &skill_id) {
        state.pinned_skill_ids.push(skill_id);
        state.pinned_skill_ids.sort();
    }
    write_skill_curator_state(store, state)
}

pub fn unpin_skill_for_curator(store: &AppStore, selector: &str) -> AppResult<SkillCuratorState> {
    let skills = list_skills(store)?;
    let Some(skill_id) = resolve_skill_selector(&skills, selector)? else {
        return Err(AppError::NotFound(format!("skill {selector}")));
    };
    let mut state = read_skill_curator_state(store)?;
    state.pinned_skill_ids.retain(|id| id != &skill_id);
    write_skill_curator_state(store, state)
}

pub fn archive_skill_for_curator(
    store: &AppStore,
    selector: &str,
    reason: Option<&str>,
) -> AppResult<SkillCuratorArchiveRecord> {
    let (record, remove_from_lock) = match select_skill_install_records(store, Some(selector)) {
        Ok(mut records) if records.len() == 1 => (records.remove(0), true),
        Ok(records) if records.len() > 1 => {
            return Err(AppError::BadRequest(format!(
                "skill selector must match exactly one skill: {selector}"
            )))
        }
        _ => {
            let skills = list_skills(store)?;
            let Some(skill_id) = resolve_skill_selector(&skills, selector)? else {
                return Err(AppError::NotFound(format!("skill {selector}")));
            };
            let Some(skill) = skills.iter().find(|skill| skill.id == skill_id) else {
                return Err(AppError::NotFound(format!("skill {selector}")));
            };
            if skill.is_core || skill.is_bundled {
                return Err(AppError::BadRequest(format!(
                    "refusing to archive bundled/core skill '{}'",
                    skill.id
                )));
            }
            (
                SkillInstallRecord {
                    skill_id: skill.id.clone(),
                    name: skill.name.clone(),
                    source: skill.source.clone(),
                    identifier: skill.path.clone(),
                    install_path: skill.path.clone(),
                    audit_status: "unknown".into(),
                    installed_at: now_iso(),
                },
                false,
            )
        }
    };
    let mut state = read_skill_curator_state(store)?;
    if state
        .pinned_skill_ids
        .iter()
        .any(|id| id == &record.skill_id)
    {
        return Err(AppError::BadRequest(format!(
            "skill {} is pinned; unpin before archiving",
            record.skill_id
        )));
    }
    let original_path = PathBuf::from(&record.install_path);
    let Some(original_dir) = original_path.parent() else {
        return Err(AppError::BadRequest(
            "skill install path has no parent".into(),
        ));
    };
    let skills_root = store.data_dir().join("skills");
    if !original_dir.starts_with(&skills_root)
        || original_dir.starts_with(skills_root.join("curator"))
    {
        return Err(AppError::BadRequest(format!(
            "refusing to archive skill outside managed skills dir: {}",
            original_dir.to_string_lossy()
        )));
    }
    let archive_id = format!(
        "archive-{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        slug(&record.skill_id.replace('/', "-"))
    );
    let archive_dir = store
        .data_dir()
        .join("skills")
        .join("curator")
        .join("archive")
        .join(&archive_id);
    fs::create_dir_all(archive_dir.parent().unwrap_or_else(|| Path::new(".")))?;
    fs::rename(original_dir, &archive_dir)?;
    store.remove_skill(&record.skill_id)?;
    if remove_from_lock {
        let mut lock_records = read_skill_install_records(store)?;
        lock_records.retain(|item| item.skill_id != record.skill_id);
        write_skill_install_records(store, &lock_records)?;
    }

    let archived = SkillCuratorArchiveRecord {
        archive_id,
        skill_id: record.skill_id.clone(),
        name: record.name.clone(),
        original_path: record.install_path.clone(),
        archive_path: archive_dir.to_string_lossy().to_string(),
        reason: reason
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Archived by SynthChat skill curator.")
            .to_string(),
        archived_at: now_iso(),
        restored_at: None,
        install_record: record,
    };
    state.archived.push(archived.clone());
    write_skill_curator_state(store, state)?;
    Ok(archived)
}

pub fn restore_skill_for_curator(
    store: &AppStore,
    archive_selector: &str,
) -> AppResult<SkillCuratorArchiveRecord> {
    let mut state = read_skill_curator_state(store)?;
    let matches = state
        .archived
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.restored_at.is_none()
                && (record.archive_id == archive_selector
                    || record.archive_id.starts_with(archive_selector)
                    || record.skill_id == archive_selector
                    || record.skill_id.starts_with(archive_selector))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let index = match matches.as_slice() {
        [index] => *index,
        [] => return Err(AppError::NotFound(format!("archive {archive_selector}"))),
        _ => {
            return Err(AppError::BadRequest(format!(
                "archive selector is ambiguous: {archive_selector}"
            )))
        }
    };
    let mut record = state.archived[index].clone();
    let archive_dir = PathBuf::from(&record.archive_path);
    let original_path = PathBuf::from(&record.original_path);
    let Some(original_dir) = original_path.parent() else {
        return Err(AppError::BadRequest(
            "archived skill path has no parent".into(),
        ));
    };
    if original_dir.exists() {
        return Err(AppError::BadRequest(format!(
            "cannot restore; target already exists: {}",
            original_dir.to_string_lossy()
        )));
    }
    fs::create_dir_all(original_dir.parent().unwrap_or_else(|| Path::new(".")))?;
    fs::rename(&archive_dir, original_dir)?;

    let restored_skill = skill_summary_for_installed_path(
        &original_path,
        &record.name,
        &record.skill_id,
        &record.install_record.source,
    );
    let mut skills = store.skills()?;
    skills.retain(|skill| skill.id != restored_skill.id);
    skills.push(restored_skill);
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    store.set_skills(skills)?;
    if record.install_record.source == "external" {
        let mut lock_records = read_skill_install_records(store)?;
        lock_records.retain(|item| item.skill_id != record.install_record.skill_id);
        lock_records.push(record.install_record.clone());
        lock_records.sort_by(|a, b| a.skill_id.cmp(&b.skill_id));
        write_skill_install_records(store, &lock_records)?;
    }

    record.restored_at = Some(now_iso());
    state.archived[index] = record.clone();
    write_skill_curator_state(store, state)?;
    Ok(record)
}

pub fn install_external_skill_file(
    store: &AppStore,
    source_path: &str,
    name_override: Option<&str>,
    category: Option<&str>,
    agent_id: Option<&str>,
    force: bool,
) -> AppResult<EnhancedSkillSummary> {
    let source = PathBuf::from(source_path);
    if source.is_dir() {
        return install_external_skill_directory(
            store,
            &source,
            name_override,
            category,
            agent_id,
            force,
        );
    }
    if !source.is_file() {
        return Err(AppError::NotFound(format!("skill file or directory {source_path}")));
    }
    let raw = fs::read_to_string(&source)?;
    let fallback_name = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("external-skill");
    install_external_skill_content(
        store,
        &raw,
        fallback_name,
        name_override,
        category,
        agent_id,
        force,
        false,
        source_path,
    )
}

fn install_external_skill_directory(
    store: &AppStore,
    source_dir: &Path,
    name_override: Option<&str>,
    category: Option<&str>,
    agent_id: Option<&str>,
    force: bool,
) -> AppResult<EnhancedSkillSummary> {
    let source_skill_path = source_dir.join("SKILL.md");
    if !source_skill_path.is_file() {
        return Err(AppError::NotFound(format!(
            "skill directory missing SKILL.md: {}",
            source_dir.to_string_lossy()
        )));
    }
    let raw = fs::read_to_string(&source_skill_path)?;
    let metadata = frontmatter(&raw);
    let name = name_override
        .map(clean_meta_value)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| metadata.get("name").cloned())
        .or_else(|| heading(&raw))
        .unwrap_or_else(|| {
            source_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("external-skill")
                .to_string()
        });
    let skill_slug = slug(&name);
    if skill_slug.is_empty() {
        return Err(AppError::BadRequest("invalid skill name".into()));
    }
    let category_slug = category
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(['/', '\\'])
                .map(slug)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|value| !value.is_empty());

    let data_dir = store.data_dir();
    let quarantine_dir = data_dir
        .join("skills")
        .join("quarantine")
        .join(new_id("skill"));
    copy_dir_contents(source_dir, &quarantine_dir)?;
    let quarantine_skill_path = quarantine_dir.join("SKILL.md");
    let mut staged = skill_summary_for_installed_path(
        &quarantine_skill_path,
        &name,
        "external/quarantine",
        "quarantine",
    );
    let audit = audit_skill(staged.clone());
    let blocked = audit
        .findings
        .iter()
        .any(|finding| finding.severity == "critical" || finding.severity == "high");
    if blocked && !force {
        let categories = audit
            .findings
            .iter()
            .map(|finding| finding.category.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let _ = fs::remove_dir_all(&quarantine_dir);
        return Err(AppError::BadRequest(format!(
            "skill audit blocked install: {categories}; pass force to override"
        )));
    }

    let mut install_dir = data_dir.join("skills").join("external");
    let mut id_parts = vec!["external".to_string()];
    if let Some(category_slug) = &category_slug {
        for part in category_slug.split('/') {
            install_dir = install_dir.join(part);
            id_parts.push(part.to_string());
        }
    }
    install_dir = install_dir.join(&skill_slug);
    id_parts.push(skill_slug);
    let install_skill_path = install_dir.join("SKILL.md");
    if install_skill_path.exists() && !force {
        let _ = fs::remove_dir_all(&quarantine_dir);
        return Err(AppError::BadRequest(format!(
            "skill already exists at {}",
            install_skill_path.to_string_lossy()
        )));
    }
    if install_dir.exists() {
        let _ = fs::remove_dir_all(&install_dir);
    }
    copy_dir_contents(&quarantine_dir, &install_dir)?;
    let _ = fs::remove_dir_all(&quarantine_dir);

    staged = skill_summary_for_installed_path(
        &install_skill_path,
        &name,
        &id_parts.join("/"),
        "external",
    );
    let mut skills = list_skills(store)?;
    skills.retain(|skill| skill.id != staged.id);
    skills.push(staged.clone());
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    store.set_skills(skills)?;
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        store.enable_agent_skills(agent_id, vec![staged.id.clone()])?;
    }
    record_skill_install(
        store,
        &staged,
        &source_dir.to_string_lossy(),
        &audit,
    )?;
    Ok(staged)
}

pub fn install_external_skill_content(
    store: &AppStore,
    raw: &str,
    fallback_name: &str,
    name_override: Option<&str>,
    category: Option<&str>,
    agent_id: Option<&str>,
    force: bool,
    allow_existing: bool,
    identifier: &str,
) -> AppResult<EnhancedSkillSummary> {
    let metadata = frontmatter(&raw);
    let name = name_override
        .map(clean_meta_value)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| metadata.get("name").cloned())
        .or_else(|| heading(&raw))
        .unwrap_or_else(|| fallback_name.to_string());
    let skill_slug = slug(&name);
    if skill_slug.is_empty() {
        return Err(AppError::BadRequest("invalid skill name".into()));
    }
    let category_slug = category
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(['/', '\\'])
                .map(slug)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|value| !value.is_empty());
    let data_dir = store.data_dir();
    let quarantine_dir = data_dir
        .join("skills")
        .join("quarantine")
        .join(new_id("skill"));
    fs::create_dir_all(&quarantine_dir)?;
    let quarantine_skill_path = quarantine_dir.join("SKILL.md");
    fs::write(&quarantine_skill_path, raw.as_bytes())?;

    let mut staged = skill_summary_for_installed_path(
        &quarantine_skill_path,
        &name,
        "external/quarantine",
        "quarantine",
    );
    let audit = audit_skill(staged.clone());
    let blocked = audit
        .findings
        .iter()
        .any(|finding| finding.severity == "critical" || finding.severity == "high");
    if blocked && !force {
        let categories = audit
            .findings
            .iter()
            .map(|finding| finding.category.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let _ = fs::remove_dir_all(&quarantine_dir);
        return Err(AppError::BadRequest(format!(
            "skill audit blocked install: {categories}; pass force to override"
        )));
    }

    let mut install_dir = data_dir.join("skills").join("external");
    let mut id_parts = vec!["external".to_string()];
    if let Some(category_slug) = &category_slug {
        for part in category_slug.split('/') {
            install_dir = install_dir.join(part);
            id_parts.push(part.to_string());
        }
    }
    install_dir = install_dir.join(&skill_slug);
    id_parts.push(skill_slug);
    let install_skill_path = install_dir.join("SKILL.md");
    if install_skill_path.exists() && !force && !allow_existing {
        let _ = fs::remove_dir_all(&quarantine_dir);
        return Err(AppError::BadRequest(format!(
            "skill already exists at {}",
            install_skill_path.to_string_lossy()
        )));
    }
    fs::create_dir_all(&install_dir)?;
    fs::copy(&quarantine_skill_path, &install_skill_path)?;
    let _ = fs::remove_dir_all(&quarantine_dir);

    staged = skill_summary_for_installed_path(
        &install_skill_path,
        &name,
        &id_parts.join("/"),
        "external",
    );
    let mut skills = list_skills(store)?;
    skills.retain(|skill| skill.id != staged.id);
    skills.push(staged.clone());
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    store.set_skills(skills)?;
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        store.enable_agent_skills(agent_id, vec![staged.id.clone()])?;
    }
    record_skill_install(store, &staged, identifier, &audit)?;
    Ok(staged)
}

pub fn skill_install_records(store: &AppStore) -> AppResult<Vec<SkillInstallRecord>> {
    read_skill_install_records(store)
}

pub fn skill_usage_records(store: &AppStore) -> AppResult<Vec<Value>> {
    let mut records = read_skill_usage_records(store)?
        .into_values()
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right
            .get("lastUsedAt")
            .and_then(Value::as_str)
            .cmp(&left.get("lastUsedAt").and_then(Value::as_str))
            .then_with(|| {
                left.get("skillId")
                    .and_then(Value::as_str)
                    .cmp(&right.get("skillId").and_then(Value::as_str))
            })
    });
    Ok(records)
}

pub fn record_skill_usage(
    store: &AppStore,
    skill: &EnhancedSkillSummary,
    context: &str,
    origin: Option<&str>,
) -> AppResult<Value> {
    let mut records = read_skill_usage_records(store)?;
    let now = now_iso();
    let context = normalize_skill_usage_context(context);
    let origin = origin
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("foreground");
    let mut record = records.remove(&skill.id).unwrap_or_else(|| {
        json!({
            "skillId": skill.id,
            "name": skill.name,
            "path": skill.path,
            "source": skill.source,
            "firstSeenAt": now,
            "lastUsedAt": null,
            "useCount": 0,
            "contexts": {},
            "provenance": {
                "origin": origin,
                "agentCreated": skill.source == "agent-managed" || skill.id.contains("agent-managed"),
                "backgroundCreated": origin == "background_review" || skill.id.contains("background-review")
            }
        })
    });
    record["name"] = json!(skill.name);
    record["path"] = json!(skill.path);
    record["source"] = json!(skill.source);
    record["lastUsedAt"] = json!(now);
    record["lastContext"] = json!(context);
    record["lastOrigin"] = json!(origin);
    let use_count = record
        .get("useCount")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    record["useCount"] = json!(use_count);
    let mut contexts = record
        .get("contexts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let next_context_count = contexts
        .get(context)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    contexts.insert(context.to_string(), json!(next_context_count));
    record["contexts"] = json!(contexts);
    if let Some(provenance) = record.get_mut("provenance").and_then(Value::as_object_mut) {
        provenance.insert("origin".into(), json!(origin));
        provenance.insert(
            "agentCreated".into(),
            json!(
                provenance
                    .get("agentCreated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || skill.source == "agent-managed"
                    || skill.id.contains("agent-managed")
            ),
        );
        provenance.insert(
            "backgroundCreated".into(),
            json!(
                provenance
                    .get("backgroundCreated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || origin == "background_review"
                    || skill.id.contains("background-review")
            ),
        );
    }
    records.insert(skill.id.clone(), record.clone());
    write_skill_usage_records(store, &records)?;
    Ok(record)
}

pub fn skill_audit_log(
    store: &AppStore,
    limit: Option<usize>,
) -> AppResult<Vec<serde_json::Value>> {
    let path = store.data_dir().join("skills").join("audit-log.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let mut entries = Vec::new();
    for line in raw.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            entries.push(value);
            if entries.len() >= limit {
                break;
            }
        }
    }
    Ok(entries)
}

pub fn list_skill_taps(store: &AppStore) -> AppResult<Vec<SkillTap>> {
    read_skill_taps(store)
}

pub fn add_skill_tap(store: &AppStore, repo: &str, path: Option<&str>) -> AppResult<SkillTap> {
    let repo = normalize_tap_repo(repo)?;
    let path = normalize_tap_path(path.unwrap_or("skills/"))?;
    let mut taps = read_skill_taps(store)?;
    if let Some(existing) = taps
        .iter()
        .find(|tap| tap.repo.eq_ignore_ascii_case(&repo))
        .cloned()
    {
        return Ok(existing);
    }
    let tap = SkillTap { repo, path };
    taps.push(tap.clone());
    taps.sort_by(|a, b| a.repo.cmp(&b.repo).then_with(|| a.path.cmp(&b.path)));
    write_skill_taps(store, &taps)?;
    Ok(tap)
}

pub fn remove_skill_tap(store: &AppStore, repo: &str) -> AppResult<bool> {
    let repo = repo.trim().to_lowercase();
    if repo.is_empty() {
        return Err(AppError::BadRequest("tap repo is required".into()));
    }
    let taps = read_skill_taps(store)?;
    let mut removed = false;
    let next = taps
        .into_iter()
        .filter(|tap| {
            let keep = tap.repo.to_lowercase() != repo;
            if !keep {
                removed = true;
            }
            keep
        })
        .collect::<Vec<_>>();
    if removed {
        write_skill_taps(store, &next)?;
    }
    Ok(removed)
}

pub fn check_skill_updates(
    store: &AppStore,
    selector: Option<&str>,
) -> AppResult<Vec<SkillUpdateCheck>> {
    let records = read_skill_install_records(store)?;
    let targets = if let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty())
    {
        let selector = selector.to_lowercase();
        records
            .into_iter()
            .filter(|record| {
                record.skill_id.to_lowercase().starts_with(&selector)
                    || record.name.to_lowercase().starts_with(&selector)
            })
            .collect::<Vec<_>>()
    } else {
        records
    };
    Ok(targets
        .into_iter()
        .map(|record| check_skill_update_record(&record))
        .collect())
}

pub fn update_skills_from_sources(
    store: &AppStore,
    selector: Option<&str>,
    agent_id: Option<&str>,
    force: bool,
) -> AppResult<Vec<EnhancedSkillSummary>> {
    let records = select_skill_install_records(store, selector)?;
    let mut updated = Vec::new();
    for record in records {
        if record.identifier.starts_with("http://") || record.identifier.starts_with("https://") {
            continue;
        }
        let source_root = PathBuf::from(&record.identifier);
        let Some(source_path) = source_skill_markdown_path(&record.identifier) else {
            continue;
        };
        let category = category_from_external_skill_id(&record.skill_id);
        let skill = if source_root.is_dir() {
            install_external_skill_directory(
                store,
                &source_root,
                Some(&record.name),
                category.as_deref(),
                agent_id,
                force,
            )?
        } else {
            let raw = fs::read_to_string(&source_path)?;
            install_external_skill_content(
                store,
                &raw,
                &record.name,
                Some(&record.name),
                category.as_deref(),
                agent_id,
                force,
                true,
                &record.identifier,
            )?
        };
        updated.push(skill);
    }
    Ok(updated)
}

pub fn uninstall_external_skills(
    store: &AppStore,
    selector: Option<&str>,
    remove_files: bool,
) -> AppResult<Vec<SkillInstallRecord>> {
    let records = select_skill_install_records(store, selector)?;
    let mut lock_records = read_skill_install_records(store)?;
    for record in &records {
        store.remove_skill(&record.skill_id)?;
        lock_records.retain(|item| item.skill_id != record.skill_id);
        if remove_files {
            remove_external_skill_files(store, record)?;
        }
        append_skill_uninstall_log(store, record, remove_files)?;
    }
    write_skill_install_records(store, &lock_records)?;
    Ok(records)
}

pub fn export_skill_snapshot(store: &AppStore, path: &str) -> AppResult<String> {
    let target = PathBuf::from(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let snapshot = json!({
        "schema": "synthchat.skills.snapshot.v1",
        "exportedAt": now_iso(),
        "skills": list_skills(store)?,
        "installRecords": read_skill_install_records(store)?,
        "taps": read_skill_taps(store)?,
    });
    fs::write(&target, serde_json::to_vec_pretty(&snapshot)?)?;
    Ok(target.to_string_lossy().to_string())
}

pub fn import_skill_snapshot(store: &AppStore, path: &str) -> AppResult<usize> {
    let raw = fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let skills = serde_json::from_value::<Vec<EnhancedSkillSummary>>(
        value
            .get("skills")
            .cloned()
            .ok_or_else(|| AppError::BadRequest("skills snapshot missing skills".into()))?,
    )?;
    let records = serde_json::from_value::<Vec<SkillInstallRecord>>(
        value
            .get("installRecords")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    let taps = serde_json::from_value::<Vec<SkillTap>>(
        value.get("taps").cloned().unwrap_or_else(|| json!([])),
    )?;
    let count = skills.len();
    store.set_skills(skills)?;
    write_skill_install_records(store, &records)?;
    write_skill_taps(store, &taps)?;
    Ok(count)
}

fn select_skill_install_records(
    store: &AppStore,
    selector: Option<&str>,
) -> AppResult<Vec<SkillInstallRecord>> {
    let records = read_skill_install_records(store)?;
    if let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) {
        let selector = selector.to_lowercase();
        let matches = records
            .into_iter()
            .filter(|record| {
                record.skill_id.to_lowercase().starts_with(&selector)
                    || record.name.to_lowercase().starts_with(&selector)
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(AppError::NotFound(format!(
                "skill install record {selector}"
            )));
        }
        return Ok(matches);
    }
    Ok(records)
}

fn remove_external_skill_files(store: &AppStore, record: &SkillInstallRecord) -> AppResult<()> {
    let base = store.data_dir().join("skills").join("external");
    let install_path = PathBuf::from(&record.install_path);
    let Some(dir) = install_path.parent() else {
        return Ok(());
    };
    if !dir.starts_with(&base) {
        return Err(AppError::BadRequest(format!(
            "refusing to remove skill outside external skills dir: {}",
            dir.to_string_lossy()
        )));
    }
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn detect_skill_overlap_clusters(skills: &[EnhancedSkillSummary]) -> Vec<SkillCuratorOverlap> {
    let mut grouped: BTreeMap<String, Vec<&EnhancedSkillSummary>> = BTreeMap::new();
    for skill in skills.iter().filter(|skill| !skill.is_bundled) {
        for token in curator_skill_tokens(skill).into_iter().take(5) {
            grouped.entry(token).or_default().push(skill);
        }
    }
    let mut clusters = Vec::new();
    let mut seen_signatures = HashSet::new();
    for (umbrella, mut items) in grouped {
        items.sort_by(|a, b| a.id.cmp(&b.id));
        items.dedup_by(|a, b| a.id == b.id);
        if items.len() < 2 {
            continue;
        }
        let ids = items
            .iter()
            .map(|skill| skill.id.clone())
            .collect::<Vec<_>>();
        let signature = ids.join("|");
        if !seen_signatures.insert(signature) {
            continue;
        }
        clusters.push(SkillCuratorOverlap {
            umbrella: title_case(&umbrella.replace('-', " ")),
            reason: format!(
                "External skills share the `{umbrella}` topic token; inspect whether they should become one umbrella skill."
            ),
            skill_ids: ids,
        });
        if clusters.len() >= 12 {
            break;
        }
    }
    clusters
}

fn detect_skill_archive_candidates(
    skills: &[EnhancedSkillSummary],
    overlap_clusters: &[SkillCuratorOverlap],
    state: &SkillCuratorState,
    usage: &BTreeMap<String, Value>,
) -> Vec<SkillCuratorArchiveCandidate> {
    let mut duplicate_members = HashSet::new();
    for cluster in overlap_clusters {
        for skill_id in cluster.skill_ids.iter().skip(1) {
            duplicate_members.insert(skill_id.clone());
        }
    }
    skills
        .iter()
        .filter(|skill| !skill.is_bundled && duplicate_members.contains(&skill.id))
        .filter(|skill| !state.pinned_skill_ids.iter().any(|id| id == &skill.id))
        .filter(|skill| {
            usage
                .get(&skill.id)
                .and_then(|record| record.get("useCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0
        })
        .take(20)
        .map(|skill| SkillCuratorArchiveCandidate {
            skill_id: skill.id.clone(),
            name: skill.name.clone(),
            reason: "Potential sibling in an overlap cluster; archive only after merging unique content into an umbrella.".into(),
        })
        .collect()
}

fn skill_curator_state_path(store: &AppStore) -> PathBuf {
    store
        .data_dir()
        .join("skills")
        .join("curator")
        .join("state.json")
}

fn default_skill_curator_state() -> SkillCuratorState {
    SkillCuratorState {
        paused: false,
        pinned_skill_ids: Vec::new(),
        archived: Vec::new(),
        last_run_at: None,
        last_report_path: None,
        run_count: 0,
        updated_at: now_iso(),
    }
}

fn read_skill_curator_state(store: &AppStore) -> AppResult<SkillCuratorState> {
    let path = skill_curator_state_path(store);
    if !path.exists() {
        return Ok(default_skill_curator_state());
    }
    let raw = fs::read_to_string(path)?;
    let mut state = serde_json::from_str::<SkillCuratorState>(&raw)
        .unwrap_or_else(|_| default_skill_curator_state());
    state.pinned_skill_ids.sort();
    state.pinned_skill_ids.dedup();
    Ok(state)
}

fn write_skill_curator_state(
    store: &AppStore,
    mut state: SkillCuratorState,
) -> AppResult<SkillCuratorState> {
    state.updated_at = now_iso();
    state.pinned_skill_ids.sort();
    state.pinned_skill_ids.dedup();
    let path = skill_curator_state_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(&state)?)?;
    Ok(state)
}

fn curator_skill_tokens(skill: &EnhancedSkillSummary) -> Vec<String> {
    let text = format!("{} {} {}", skill.id, skill.name, skill.description).to_lowercase();
    let stop = [
        "external",
        "background",
        "review",
        "skill",
        "skills",
        "use",
        "when",
        "with",
        "from",
        "this",
        "that",
        "into",
        "task",
        "workflow",
        "guidance",
    ];
    let mut counts = BTreeMap::<String, usize>::new();
    for raw in text.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        let token = raw.trim();
        if token.len() < 4 || stop.contains(&token) {
            continue;
        }
        *counts.entry(token.to_string()).or_default() += 1;
    }
    let mut tokens = counts.into_iter().collect::<Vec<_>>();
    tokens.sort_by(|(left_token, left_count), (right_token, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_token.cmp(right_token))
    });
    tokens.into_iter().map(|(token, _)| token).collect()
}

fn format_skill_curator_markdown(report: &SkillCuratorReport) -> String {
    let mut lines = vec![
        "# SynthChat Skill Curator Report".into(),
        String::new(),
        format!("- generated: {}", report.generated_at),
        format!("- total skills: {}", report.total_skills),
        format!("- external skills: {}", report.external_skills),
        format!("- bundled skills: {}", report.bundled_skills),
        format!("- audit attention: {}", report.audit_attention),
        String::new(),
        "## Recommendations".into(),
        String::new(),
    ];
    for item in &report.recommendations {
        lines.push(format!("- {item}"));
    }
    lines.extend([String::new(), "## Overlap Clusters".into(), String::new()]);
    if report.overlap_clusters.is_empty() {
        lines.push("- none".into());
    } else {
        for cluster in &report.overlap_clusters {
            lines.push(format!(
                "- {}: {}",
                cluster.umbrella,
                cluster.skill_ids.join(", ")
            ));
            lines.push(format!("  - reason: {}", cluster.reason));
        }
    }
    lines.extend([String::new(), "## Archive Candidates".into(), String::new()]);
    if report.archive_candidates.is_empty() {
        lines.push("- none".into());
    } else {
        for item in &report.archive_candidates {
            lines.push(format!("- {} ({})", item.name, item.skill_id));
            lines.push(format!("  - reason: {}", item.reason));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn category_from_external_skill_id(skill_id: &str) -> Option<String> {
    let parts = skill_id.split('/').collect::<Vec<_>>();
    if parts.len() <= 2 || parts.first() != Some(&"external") {
        return None;
    }
    Some(parts[1..parts.len() - 1].join("/"))
}

fn record_skill_install(
    store: &AppStore,
    skill: &EnhancedSkillSummary,
    identifier: &str,
    audit: &SkillAuditReport,
) -> AppResult<()> {
    let record = SkillInstallRecord {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        source: skill.source.clone(),
        identifier: identifier.into(),
        install_path: skill.path.clone(),
        audit_status: audit.status.clone(),
        installed_at: now_iso(),
    };
    let mut records = read_skill_install_records(store)?;
    records.retain(|item| item.skill_id != record.skill_id);
    records.push(record.clone());
    records.sort_by(|a, b| a.skill_id.cmp(&b.skill_id));
    let skills_dir = store.data_dir().join("skills");
    fs::create_dir_all(&skills_dir)?;
    fs::write(
        skills_dir.join("skills-lock.json"),
        serde_json::to_vec_pretty(&records)?,
    )?;
    let _ = record_skill_usage(store, skill, "install", Some("foreground"));
    append_skill_audit_log(store, &record, audit)
}

fn write_skill_install_records(store: &AppStore, records: &[SkillInstallRecord]) -> AppResult<()> {
    let skills_dir = store.data_dir().join("skills");
    fs::create_dir_all(&skills_dir)?;
    fs::write(
        skills_dir.join("skills-lock.json"),
        serde_json::to_vec_pretty(records)?,
    )?;
    Ok(())
}

fn check_skill_update_record(record: &SkillInstallRecord) -> SkillUpdateCheck {
    let installed_path = PathBuf::from(&record.install_path);
    if !installed_path.is_file() {
        return skill_update_check(record, "missing", "installed SKILL.md path is missing");
    }
    if record.identifier.starts_with("http://") || record.identifier.starts_with("https://") {
        return skill_update_check(
            record,
            "remote_check_required",
            "remote URL checks require async refresh",
        );
    }
    let Some(source_path) = source_skill_markdown_path(&record.identifier) else {
        return skill_update_check(
            record,
            "source_missing",
            "source skill file or directory is missing",
        );
    };
    let installed_hash = fs::read_to_string(&installed_path)
        .map(|raw| stable_text_hash(&raw))
        .ok();
    let source_hash = fs::read_to_string(&source_path)
        .map(|raw| stable_text_hash(&raw))
        .ok();
    match (installed_hash, source_hash) {
        (Some(left), Some(right)) if left == right => {
            skill_update_check(record, "current", "source matches installed content")
        }
        (Some(_), Some(_)) => {
            skill_update_check(record, "update_available", "source content differs")
        }
        _ => skill_update_check(
            record,
            "unknown",
            "could not read source or installed content",
        ),
    }
}

fn source_skill_markdown_path(identifier: &str) -> Option<PathBuf> {
    let path = PathBuf::from(identifier);
    if path.is_file() {
        return Some(path);
    }
    let skill_md = path.join("SKILL.md");
    skill_md.is_file().then_some(skill_md)
}

fn skill_update_check(record: &SkillInstallRecord, status: &str, detail: &str) -> SkillUpdateCheck {
    SkillUpdateCheck {
        skill_id: record.skill_id.clone(),
        name: record.name.clone(),
        status: status.into(),
        detail: detail.into(),
    }
}

fn stable_text_hash(raw: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut hasher);
    hasher.finish()
}

fn read_skill_install_records(store: &AppStore) -> AppResult<Vec<SkillInstallRecord>> {
    let path = store.data_dir().join("skills").join("skills-lock.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&raw)?)
}

fn skill_usage_path(store: &AppStore) -> PathBuf {
    store.data_dir().join("skills").join("usage.json")
}

fn read_skill_usage_records(store: &AppStore) -> AppResult<BTreeMap<String, Value>> {
    let path = skill_usage_path(store);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let value = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({}));
    let items = value
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| value.as_array().cloned())
        .unwrap_or_default();
    let mut records = BTreeMap::new();
    for item in items {
        if let Some(skill_id) = item.get("skillId").and_then(Value::as_str) {
            records.insert(skill_id.to_string(), item);
        }
    }
    Ok(records)
}

fn write_skill_usage_records(store: &AppStore, records: &BTreeMap<String, Value>) -> AppResult<()> {
    let path = skill_usage_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = json!({
        "schema": "synthchat.skills.usage.v1",
        "updatedAt": now_iso(),
        "records": records.values().cloned().collect::<Vec<_>>()
    });
    fs::write(path, serde_json::to_vec_pretty(&payload)?)?;
    Ok(())
}

fn normalize_skill_usage_context(context: &str) -> &str {
    match context.trim() {
        "prompt" | "view" | "install" | "create" | "edit" | "patch" | "write_file"
        | "remove_file" | "delete" | "archive" | "restore" => context.trim(),
        _ => "other",
    }
}

fn read_skill_taps(store: &AppStore) -> AppResult<Vec<SkillTap>> {
    let path = store.data_dir().join("skills").join("taps.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    Ok(serde_json::from_value(
        value.get("taps").cloned().unwrap_or_else(|| json!([])),
    )?)
}

fn write_skill_taps(store: &AppStore, taps: &[SkillTap]) -> AppResult<()> {
    let skills_dir = store.data_dir().join("skills");
    fs::create_dir_all(&skills_dir)?;
    let payload = json!({ "taps": taps });
    fs::write(
        skills_dir.join("taps.json"),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

fn normalize_tap_repo(repo: &str) -> AppResult<String> {
    let repo = repo.trim().trim_end_matches('/').to_lowercase();
    let parts = repo.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part == &"."
                || part == &".."
                || !part
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
        })
    {
        return Err(AppError::BadRequest(
            "tap repo must use owner/repo format".into(),
        ));
    }
    Ok(repo)
}

fn normalize_tap_path(path: &str) -> AppResult<String> {
    let mut path = path.trim().replace('\\', "/");
    if path.is_empty() {
        path = "skills/".into();
    }
    if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err(AppError::BadRequest("tap path must be relative".into()));
    }
    while path.starts_with("./") {
        path = path.trim_start_matches("./").to_string();
    }
    if !path.ends_with('/') {
        path.push('/');
    }
    Ok(path)
}

fn append_skill_audit_log(
    store: &AppStore,
    record: &SkillInstallRecord,
    audit: &SkillAuditReport,
) -> AppResult<()> {
    let skills_dir = store.data_dir().join("skills");
    fs::create_dir_all(&skills_dir)?;
    let entry = json!({
        "type": "skill_install",
        "createdAt": now_iso(),
        "skillId": record.skill_id,
        "name": record.name,
        "source": record.source,
        "identifier": record.identifier,
        "installPath": record.install_path,
        "auditStatus": record.audit_status,
        "findingCount": audit.findings.len(),
    });
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(skills_dir.join("audit-log.jsonl"))?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

fn append_skill_uninstall_log(
    store: &AppStore,
    record: &SkillInstallRecord,
    removed_files: bool,
) -> AppResult<()> {
    let skills_dir = store.data_dir().join("skills");
    fs::create_dir_all(&skills_dir)?;
    let entry = json!({
        "type": "skill_uninstall",
        "createdAt": now_iso(),
        "skillId": record.skill_id,
        "name": record.name,
        "source": record.source,
        "identifier": record.identifier,
        "installPath": record.install_path,
        "removedFiles": removed_files,
    });
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(skills_dir.join("audit-log.jsonl"))?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

fn resolve_skill_selector(
    skills: &[EnhancedSkillSummary],
    selector: &str,
) -> AppResult<Option<String>> {
    let selector = selector.trim().to_lowercase();
    if selector.is_empty() {
        return Ok(None);
    }
    if let Some(skill) = skills
        .iter()
        .find(|skill| skill.id.to_lowercase() == selector || skill.name.to_lowercase() == selector)
    {
        return Ok(Some(skill.id.clone()));
    }
    let matches = skills
        .iter()
        .filter(|skill| {
            skill.id.to_lowercase().starts_with(&selector)
                || skill.name.to_lowercase().starts_with(&selector)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [skill] => Ok(Some(skill.id.clone())),
        [] => Ok(None),
        _ => Err(AppError::BadRequest(format!(
            "skill selector is ambiguous: {selector}"
        ))),
    }
}

pub fn save_skill_config(
    store: &AppStore,
    agent_id: &str,
    skill_id: &str,
    config: HashMap<String, String>,
) -> AppResult<()> {
    store.save_skill_config(agent_id, skill_id, config)
}

pub fn prompt_blocks_for_request(
    store: &AppStore,
    agent: &AgentDefinition,
    user_request: &str,
) -> AppResult<Vec<SkillPromptBlock>> {
    if !agent.skills_enabled {
        return Ok(vec![]);
    }

    let skills = list_skills(store)?;
    let requested = requested_skill_names(user_request);
    let selected_skills = skills
        .into_iter()
        .filter(|skill| {
            agent.enabled_skills.contains(&skill.id)
                || requested.contains(&skill.name.to_lowercase())
                || requested.contains(&skill.id.to_lowercase())
        })
        .take(6)
        .collect::<Vec<_>>();
    let mut selected = Vec::new();
    for skill in selected_skills {
        let Ok(content) = fs::read_to_string(&skill.path) else {
            continue;
        };
        let _ = record_skill_usage(store, &skill, "prompt", Some("foreground"));
        selected.push(SkillPromptBlock {
            id: skill.id,
            name: skill.name,
            content: truncate_chars(content, MAX_SKILL_PROMPT_CHARS),
        });
    }
    Ok(selected)
}

fn marketplace_skill_from_summary(skill: EnhancedSkillSummary) -> MarketplaceSkill {
    let mut tags = skill
        .id
        .split('/')
        .take(3)
        .chain(skill.source.split(['-', '_', '/']).take(2))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    MarketplaceSkill {
        id: skill.id,
        name: skill.name,
        description: skill.description,
        version: skill.version,
        author: skill.author,
        download_url: skill.path,
        icon: skill.icon,
        tags,
    }
}

pub fn marketplace_skill_from_remote_content(
    id: String,
    raw: &str,
    download_url: String,
    tap: &SkillTap,
) -> MarketplaceSkill {
    let metadata = frontmatter(raw);
    let name = metadata
        .get("name")
        .cloned()
        .or_else(|| heading(raw))
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or("remote-skill").to_string());
    let description = metadata
        .get("description")
        .cloned()
        .or_else(|| first_paragraph(raw))
        .unwrap_or_default();
    let mut tags = vec![
        "tap".to_string(),
        "github".to_string(),
        tap.repo.replace('/', "-"),
    ];
    tags.extend(
        id.split('/')
            .take(4)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_lowercase()),
    );
    tags.sort();
    tags.dedup();
    MarketplaceSkill {
        id,
        name,
        description: clean_meta_value(&description),
        version: metadata
            .get("version")
            .cloned()
            .unwrap_or_else(|| "1.0.0".into()),
        author: metadata.get("author").cloned().unwrap_or_default(),
        download_url,
        icon: "sparkles".into(),
        tags,
    }
}

fn skill_summary_for_installed_path(
    path: &Path,
    fallback_name: &str,
    id: &str,
    source: &str,
) -> EnhancedSkillSummary {
    let raw = fs::read_to_string(path).unwrap_or_default();
    let metadata = frontmatter(&raw);
    let name = metadata
        .get("name")
        .cloned()
        .or_else(|| heading(&raw))
        .unwrap_or_else(|| fallback_name.to_string());
    let description = metadata
        .get("description")
        .cloned()
        .or_else(|| first_paragraph(&raw))
        .unwrap_or_default();
    EnhancedSkillSummary {
        id: id.into(),
        name,
        description: clean_meta_value(&description),
        enabled: false,
        path: path.to_string_lossy().to_string(),
        version: metadata
            .get("version")
            .cloned()
            .unwrap_or_else(|| "1.0.0".into()),
        author: metadata.get("author").cloned().unwrap_or_default(),
        icon: "sparkles".into(),
        is_core: false,
        is_bundled: false,
        source: source.into(),
        agent_id: String::new(),
        config: HashMap::new(),
        required_environment_variables: parse_frontmatter_list(
            &raw,
            "required_environment_variables",
        ),
        required_credential_files: parse_frontmatter_list(&raw, "required_credential_files"),
    }
}

fn audit_skill(skill: EnhancedSkillSummary) -> SkillAuditReport {
    let mut findings = Vec::new();
    let skill_path = PathBuf::from(&skill.path);
    if !skill_path.is_file() {
        findings.push(skill_audit_finding(
            "high",
            "missing-file",
            "SKILL.md path is missing or is not a file",
            &skill.path,
            None,
        ));
        return build_skill_audit_report(skill, 0, findings);
    }

    if skill.description.trim().is_empty() {
        findings.push(skill_audit_finding(
            "low",
            "metadata",
            "Skill description is empty",
            &skill.path,
            None,
        ));
    }
    if skill.name.trim().is_empty() {
        findings.push(skill_audit_finding(
            "medium",
            "metadata",
            "Skill name is empty",
            &skill.path,
            None,
        ));
    }

    let root = skill_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let files = collect_skill_audit_files(&root);
    if files.len() >= MAX_AUDIT_FILES {
        findings.push(skill_audit_finding(
            "low",
            "coverage",
            "Audit file limit reached; some files may not have been scanned",
            &root.to_string_lossy(),
            None,
        ));
    }

    let mut checked_files = 0usize;
    for file in files.into_iter().take(MAX_AUDIT_FILES) {
        let Ok(metadata) = fs::metadata(&file) else {
            continue;
        };
        if metadata.len() > MAX_AUDIT_FILE_BYTES || !is_text_audit_file(&file) {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&file) else {
            continue;
        };
        checked_files += 1;
        findings.extend(scan_skill_audit_text(&file, &raw));
    }

    build_skill_audit_report(skill, checked_files, findings)
}

fn build_skill_audit_report(
    skill: EnhancedSkillSummary,
    checked_files: usize,
    findings: Vec<SkillAuditFinding>,
) -> SkillAuditReport {
    let status = if findings
        .iter()
        .any(|finding| finding.severity == "critical" || finding.severity == "high")
    {
        "attention"
    } else if findings.is_empty() {
        "ok"
    } else {
        "warn"
    };
    SkillAuditReport {
        skill_id: skill.id,
        name: skill.name,
        path: skill.path,
        status: status.into(),
        checked_files,
        findings,
    }
}

fn collect_skill_audit_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if file_name.starts_with('.') || matches!(file_name, "node_modules" | "target") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
            if found.len() >= MAX_AUDIT_FILES {
                found.sort();
                return found;
            }
        }
    }
    found.sort();
    found
}

fn is_text_audit_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "md" | "txt"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "rs"
            | "sh"
            | "ps1"
            | "bat"
            | "cmd"
    )
}

fn scan_skill_audit_text(path: &Path, raw: &str) -> Vec<SkillAuditFinding> {
    let mut findings = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let lower = line.to_lowercase();
        let line_no = Some(index + 1);
        if lower.contains("ignore previous instructions")
            || lower.contains("ignore all previous")
            || lower.contains("system prompt")
        {
            findings.push(skill_audit_finding(
                "high",
                "prompt-injection",
                "Prompt override or system-prompt instruction detected",
                &path.to_string_lossy(),
                line_no,
            ));
        }
        if lower.contains("reveal secrets")
            || lower.contains("print secrets")
            || lower.contains("dump env")
            || lower.contains(".env")
        {
            findings.push(skill_audit_finding(
                "high",
                "secret-access",
                "Secret or environment disclosure instruction detected",
                &path.to_string_lossy(),
                line_no,
            ));
        }
        if lower.contains("rm -rf")
            || lower.contains("remove-item")
            || lower.contains("format-volume")
            || lower.contains("del /")
        {
            findings.push(skill_audit_finding(
                "critical",
                "destructive-command",
                "Potentially destructive filesystem command detected",
                &path.to_string_lossy(),
                line_no,
            ));
        }
        if (lower.contains("curl") || lower.contains("wget") || lower.contains("irm "))
            && (lower.contains("| sh")
                || lower.contains("|sh")
                || lower.contains("iex")
                || lower.contains("invoke-expression"))
        {
            findings.push(skill_audit_finding(
                "high",
                "remote-execution",
                "Network download piped into command execution detected",
                &path.to_string_lossy(),
                line_no,
            ));
        }
    }
    findings
}

fn skill_audit_finding(
    severity: &str,
    category: &str,
    message: &str,
    file: &str,
    line: Option<usize>,
) -> SkillAuditFinding {
    SkillAuditFinding {
        severity: severity.into(),
        category: category.into(),
        message: message.into(),
        file: file.into(),
        line,
    }
}

#[derive(Debug, Clone)]
struct SkillRoot {
    path: PathBuf,
    source: String,
}

fn bundled_skill_catalog_needs_refresh(skills: &[EnhancedSkillSummary]) -> bool {
    skills.iter().any(|skill| {
        skill.is_bundled
            && (!PathBuf::from(skill.path.trim()).is_file()
                || legacy_hermes_skill_path(&skill.path))
    })
}

fn legacy_hermes_skill_path(path: &str) -> bool {
    path.replace('\\', "/")
        .to_ascii_lowercase()
        .contains("/hermes-agent/skills/")
}

fn discover_skill_roots(store: &AppStore) -> Vec<SkillRoot> {
    let mut roots = Vec::new();
    if let Ok(agent) = store.agent(None) {
        let configured = agent.skills_dir.trim();
        if !configured.is_empty() {
            roots.push(skill_root(PathBuf::from(configured), "configured-directory"));
        }
    }
    if let Ok(configured) = env::var("SYNTHCHAT_SKILLS_DIR") {
        let configured = configured.trim();
        if !configured.is_empty() {
            roots.push(skill_root(PathBuf::from(configured), "configured-directory"));
        }
    }
    roots.push(skill_root(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("skills"),
        "project",
    ));
    if let Ok(current) = env::current_dir() {
        roots.push(skill_root(current.join("skills"), "project"));
        roots.push(skill_root(current.join("..").join("skills"), "project"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(skill_root(parent.join("skills"), "bundled"));
            roots.push(skill_root(parent.join("resources").join("skills"), "bundled"));
            if let Some(grandparent) = parent.parent() {
                roots.push(skill_root(grandparent.join("skills"), "bundled"));
                roots.push(skill_root(
                    grandparent.join("resources").join("skills"),
                    "bundled",
                ));
            }
        }
    }

    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|root| {
            root.path.canonicalize().ok().map(|path| SkillRoot {
                path,
                source: root.source,
            })
        })
        .filter(|root| root.path.is_dir() && seen.insert(root.path.clone()))
        .collect()
}

fn skill_root(path: PathBuf, source: &str) -> SkillRoot {
    SkillRoot {
        path,
        source: source.into(),
    }
}

fn find_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
                found.push(path);
            }
        }
    }
    found
}

fn summarize_skill(root: &Path, path: &Path, source: &str) -> Option<EnhancedSkillSummary> {
    let raw = fs::read_to_string(path).ok()?;
    let metadata = frontmatter(&raw);
    let parent = path.parent()?;
    let rel = parent.strip_prefix(root).unwrap_or(parent);
    let id = path_to_id(rel);
    let name = metadata
        .get("name")
        .cloned()
        .or_else(|| heading(&raw))
        .unwrap_or_else(|| {
            rel.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("skill")
                .to_string()
        });
    let description = metadata
        .get("description")
        .cloned()
        .or_else(|| first_paragraph(&raw))
        .unwrap_or_default();

    Some(EnhancedSkillSummary {
        id,
        name,
        description: clean_meta_value(&description),
        enabled: false,
        path: path.to_string_lossy().to_string(),
        version: metadata
            .get("version")
            .cloned()
            .unwrap_or_else(|| "1.0.0".into()),
        author: metadata.get("author").cloned().unwrap_or_default(),
        icon: "sparkles".into(),
        is_core: false,
        is_bundled: true,
        source: source.into(),
        agent_id: String::new(),
        config: HashMap::new(),
        required_environment_variables: parse_frontmatter_list(
            &raw,
            "required_environment_variables",
        ),
        required_credential_files: parse_frontmatter_list(&raw, "required_credential_files"),
    })
}

fn copy_dir_contents(source: &Path, destination: &Path) -> AppResult<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_contents(&source_path, &destination_path)?;
        } else if source_path.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn frontmatter(raw: &str) -> HashMap<String, String> {
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
            map.insert(key.trim().to_string(), clean_meta_value(value.trim()));
        }
    }
    map
}

fn parse_frontmatter_list(raw: &str, key: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_frontmatter = false;
    let mut in_list = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if !in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = true;
            }
            continue;
        }
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
                        .map(clean_meta_value)
                        .filter(|value| !value.is_empty())
                        .collect();
                }
                if !inline.is_empty() {
                    return vec![clean_meta_value(inline)];
                }
            }
            continue;
        }
        if in_list {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = clean_meta_value(item.trim());
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

fn heading(raw: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(|value| value.trim().to_string())
    })
}

fn first_paragraph(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("---")
                && !line.starts_with('#')
                && !line.contains(':')
        })
        .map(|line| line.chars().take(240).collect())
}

fn requested_skill_names(input: &str) -> HashSet<String> {
    input
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('/'))
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '-' && ch != '/'))
        .filter(|token| !token.is_empty() && *token != "tool")
        .map(|token| token.to_lowercase())
        .collect()
}

fn path_to_id(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(slug)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_skill(id: &str, name: &str, description: &str) -> EnhancedSkillSummary {
        EnhancedSkillSummary {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            enabled: false,
            path: format!("{id}/SKILL.md"),
            version: "1.0.0".into(),
            author: String::new(),
            icon: "sparkles".into(),
            is_core: false,
            is_bundled: false,
            source: "external".into(),
            agent_id: String::new(),
            config: HashMap::new(),
            required_environment_variables: Vec::new(),
            required_credential_files: Vec::new(),
        }
    }

    #[test]
    fn curator_detects_external_overlap_clusters() {
        let skills = vec![
            test_skill(
                "external/background-review/provider-a",
                "Provider API Compatibility",
                "Handle provider response adapters and API shape changes.",
            ),
            test_skill(
                "external/background-review/provider-b",
                "Provider Response Debugging",
                "Debug provider response adapters and endpoint shape changes.",
            ),
            test_skill(
                "external/background-review/writing",
                "Writing Style",
                "Format concise replies.",
            ),
        ];
        let clusters = detect_skill_overlap_clusters(&skills);
        assert!(clusters.iter().any(|cluster| {
            cluster
                .skill_ids
                .iter()
                .any(|id| id.ends_with("provider-a"))
                && cluster
                    .skill_ids
                    .iter()
                    .any(|id| id.ends_with("provider-b"))
        }));
    }

    #[test]
    fn maybe_curate_skills_report_seeds_then_runs_after_interval() {
        let dir = std::env::temp_dir().join(format!("synthchat-curator-{}", new_id("test")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();

        let first = maybe_curate_skills_report(&store, 1).unwrap();
        assert!(first.is_none());
        let mut state = read_skill_curator_state(&store).unwrap();
        assert!(state.last_run_at.is_some());
        assert_eq!(state.run_count, 0);

        state.last_run_at = Some((chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339());
        write_skill_curator_state(&store, state).unwrap();
        let report = maybe_curate_skills_report(&store, 1).unwrap().unwrap();
        assert!(PathBuf::from(&report.report_path).exists());
        assert_eq!(read_skill_curator_state(&store).unwrap().run_count, 1);

        let _ = fs::remove_dir_all(dir);
    }
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else if ch == '-' || ch == '_' {
                '-'
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_meta_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn truncate_chars(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n\n[Skill content truncated]");
    truncated
}
