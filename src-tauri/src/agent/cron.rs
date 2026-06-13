use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{Persona, ScheduledAgentJob},
    store::AppStore,
};

use super::string_arg;
pub(super) fn cronjob_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list")
        .trim()
        .to_lowercase();
    let result = match action.as_str() {
        "list" => cronjob_list(store, payload)?,
        "status" => cronjob_status(store)?,
        "create" => cronjob_create(store, conversation_id, payload)?,
        "update" | "edit" => cronjob_update(store, payload)?,
        "pause" | "resume" => cronjob_set_enabled(store, payload, action == "resume")?,
        "delete" | "remove" => cronjob_delete(store, payload)?,
        "trigger" | "run" => cronjob_trigger(store, payload)?,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported cronjob action '{other}'. Use list, status, create, update, pause, resume, delete, or trigger."
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&result)?)
}

fn cronjob_list(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let mut jobs = store.scheduled_agent_jobs()?;
    jobs.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    jobs.truncate(limit);
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "list",
        "count": jobs.len(),
        "jobs": jobs
    }))
}

fn cronjob_status(store: &AppStore) -> AppResult<Value> {
    let jobs = store.scheduled_agent_jobs()?;
    let active_jobs = jobs
        .iter()
        .filter(|job| job.enabled && job.status != "completed")
        .collect::<Vec<_>>();
    let paused_count = jobs.iter().filter(|job| !job.enabled).count();
    let completed_count = jobs.iter().filter(|job| job.status == "completed").count();
    let next_run_at = active_jobs
        .iter()
        .filter_map(|job| job.next_run_at.as_deref())
        .min()
        .map(str::to_string);
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "status",
        "status": if active_jobs.is_empty() { "idle" } else { "active" },
        "count": jobs.len(),
        "activeCount": active_jobs.len(),
        "active_count": active_jobs.len(),
        "pausedCount": paused_count,
        "paused_count": paused_count,
        "completedCount": completed_count,
        "completed_count": completed_count,
        "nextRunAt": next_run_at,
        "next_run_at": next_run_at,
        "jobs": jobs
    }))
}

fn cronjob_create(store: &AppStore, conversation_id: &str, payload: &Value) -> AppResult<Value> {
    let conversation = store.conversation(conversation_id)?;
    let persona_selector = string_arg(payload, &["profile", "persona", "personaId", "persona_id"]);
    let persona = if let Some(selector) = persona_selector.as_deref() {
        resolve_cron_persona(store, selector)?
    } else {
        store
            .persona(conversation.persona_id.as_deref())
            .or_else(|_| store.persona(None))?
    };
    let agent_id = string_arg(payload, &["agentId", "agent_id", "agent"])
        .map(|selector| resolve_cron_agent_id(store, &selector))
        .transpose()?;
    let prompt = string_arg(payload, &["prompt", "task", "content"])
        .ok_or_else(|| AppError::BadRequest("cronjob create requires payload.prompt".into()))?;
    let mut job = ScheduledAgentJob {
        id: String::new(),
        name: string_arg(payload, &["name", "title"]).unwrap_or_default(),
        conversation_id: Some(conversation.id.clone()),
        persona_id: persona.id.clone(),
        profile: persona_selector
            .as_deref()
            .map(|_| persona.id.clone())
            .filter(|value| !value.trim().is_empty()),
        agent_id,
        prompt,
        skill: None,
        skills: normalize_skill_list(
            string_arg(payload, &["skill"]).as_deref(),
            payload.get("skills"),
        ),
        context_from: string_list_arg(payload, &["contextFrom", "context_from"]),
        script: string_arg(payload, &["script"]),
        no_agent: payload
            .get("noAgent")
            .or_else(|| payload.get("no_agent"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        provider: string_arg(payload, &["provider", "llmProvider", "llm_provider"]),
        model: string_arg(payload, &["model", "llmModel", "llm_model"]),
        base_url: string_arg(payload, &["baseUrl", "base_url"]),
        workdir: string_arg(payload, &["workdir", "workDir", "work_dir"]),
        timeout_seconds: u64_arg(
            payload,
            &[
                "timeoutSeconds",
                "timeout_seconds",
                "cronTimeout",
                "cron_timeout",
            ],
        ),
        script_timeout_seconds: u64_arg(
            payload,
            &[
                "scriptTimeoutSeconds",
                "script_timeout_seconds",
                "scriptTimeout",
                "script_timeout",
            ],
        ),
        deliver: string_arg(payload, &["deliver"]).or_else(|| Some("origin".into())),
        origin: payload.get("origin").cloned().or_else(|| {
            Some(json!({
                "platform": "synthchat",
                "conversationId": conversation.id.clone(),
            }))
        }),
        schedule_kind: string_arg(payload, &["scheduleKind", "schedule_kind"])
            .unwrap_or_else(|| "once".into())
            .trim()
            .to_lowercase(),
        schedule_display: String::new(),
        interval_minutes: payload
            .get("intervalMinutes")
            .or_else(|| payload.get("interval_minutes"))
            .and_then(Value::as_u64),
        cron_expr: string_arg(payload, &["cronExpr", "cron_expr"]),
        run_at: string_arg(payload, &["runAt", "run_at"]),
        repeat: payload
            .get("repeat")
            .or_else(|| payload.get("repeatTimes"))
            .or_else(|| payload.get("repeat_times"))
            .and_then(Value::as_u64),
        enabled_toolsets: string_list_arg(payload, &["enabledToolsets", "enabled_toolsets"]),
        disabled_toolsets: string_list_arg(payload, &["disabledToolsets", "disabled_toolsets"]),
        enabled: payload
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        status: "scheduled".into(),
        next_run_at: None,
        last_run_at: None,
        last_completed_at: None,
        last_run_status: None,
        last_output: None,
        last_output_path: None,
        last_error: None,
        last_delivery_error: None,
        run_count: 0,
        created_at: String::new(),
        updated_at: String::new(),
    };
    if let Some(schedule) =
        string_arg(payload, &["schedule"]).filter(|value| !value.trim().is_empty())
    {
        apply_cron_schedule_input(&mut job, &schedule)?;
        if job.schedule_display.trim().is_empty() {
            job.schedule_display = schedule;
        }
    } else if job.schedule_kind == "once" && job.run_at.as_deref().unwrap_or("").trim().is_empty() {
        job.run_at = Some(Utc::now().to_rfc3339());
    }
    let saved = store.save_scheduled_agent_job(job)?;
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "create",
        "jobId": saved.id,
        "job_id": saved.id,
        "name": saved.name,
        "schedule": saved.schedule_display,
        "nextRunAt": saved.next_run_at,
        "next_run_at": saved.next_run_at,
        "skills": saved.skills,
        "job": saved
    }))
}

fn cronjob_update(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let job_id = resolve_cronjob_id(store, payload)?;
    let mut job = store
        .scheduled_agent_jobs()?
        .into_iter()
        .find(|job| job.id == job_id)
        .ok_or_else(|| AppError::NotFound(format!("scheduled agent job not found: {job_id}")))?;
    if let Some(name) = string_arg(payload, &["name", "title"]) {
        job.name = name;
    }
    if let Some(prompt) = string_arg(payload, &["prompt", "task", "content"]) {
        job.prompt = prompt;
    }
    if payload_has_any(payload, &["profile", "persona", "personaId", "persona_id"]) {
        let selector = string_arg(payload, &["profile", "persona", "personaId", "persona_id"]);
        if let Some(selector) = selector.as_deref() {
            let persona = resolve_cron_persona(store, selector)?;
            job.persona_id = persona.id.clone();
            job.profile = Some(persona.id);
        } else {
            job.profile = None;
        }
    }
    if payload_has_any(payload, &["agentId", "agent_id", "agent"]) {
        job.agent_id = string_arg(payload, &["agentId", "agent_id", "agent"])
            .map(|selector| resolve_cron_agent_id(store, &selector))
            .transpose()?;
    }
    if payload_has_any(payload, &["skill", "skills"]) {
        job.skills = normalize_skill_list(
            string_arg(payload, &["skill"]).as_deref(),
            payload.get("skills"),
        );
    }
    if payload_has_any(payload, &["contextFrom", "context_from"]) {
        job.context_from = string_list_arg(payload, &["contextFrom", "context_from"]);
    }
    if payload_has_any(payload, &["script"]) {
        job.script = string_arg(payload, &["script"]);
    }
    if let Some(no_agent) = payload
        .get("noAgent")
        .or_else(|| payload.get("no_agent"))
        .and_then(Value::as_bool)
    {
        job.no_agent = no_agent;
    }
    if payload_has_any(payload, &["provider", "llmProvider", "llm_provider"]) {
        job.provider = string_arg(payload, &["provider", "llmProvider", "llm_provider"]);
    }
    if payload_has_any(payload, &["model", "llmModel", "llm_model"]) {
        job.model = string_arg(payload, &["model", "llmModel", "llm_model"]);
    }
    if payload_has_any(payload, &["baseUrl", "base_url"]) {
        job.base_url = string_arg(payload, &["baseUrl", "base_url"]);
    }
    if payload_has_any(payload, &["workdir", "workDir", "work_dir"]) {
        job.workdir = string_arg(payload, &["workdir", "workDir", "work_dir"]);
    }
    if payload_has_any(
        payload,
        &[
            "timeoutSeconds",
            "timeout_seconds",
            "cronTimeout",
            "cron_timeout",
        ],
    ) {
        job.timeout_seconds = u64_arg(
            payload,
            &[
                "timeoutSeconds",
                "timeout_seconds",
                "cronTimeout",
                "cron_timeout",
            ],
        );
    }
    if payload_has_any(
        payload,
        &[
            "scriptTimeoutSeconds",
            "script_timeout_seconds",
            "scriptTimeout",
            "script_timeout",
        ],
    ) {
        job.script_timeout_seconds = u64_arg(
            payload,
            &[
                "scriptTimeoutSeconds",
                "script_timeout_seconds",
                "scriptTimeout",
                "script_timeout",
            ],
        );
    }
    if payload_has_any(payload, &["deliver"]) {
        job.deliver = string_arg(payload, &["deliver"]);
    }
    if payload_has_any(payload, &["origin"]) {
        job.origin = payload.get("origin").cloned();
    }
    if payload_has_any(payload, &["enabledToolsets", "enabled_toolsets"]) {
        job.enabled_toolsets = string_list_arg(payload, &["enabledToolsets", "enabled_toolsets"]);
    }
    if payload_has_any(payload, &["disabledToolsets", "disabled_toolsets"]) {
        job.disabled_toolsets =
            string_list_arg(payload, &["disabledToolsets", "disabled_toolsets"]);
    }
    if let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) {
        job.enabled = enabled;
    }
    if let Some(schedule_display) = string_arg(payload, &["scheduleDisplay", "schedule_display"]) {
        job.schedule_display = schedule_display;
    }
    if let Some(schedule) =
        string_arg(payload, &["schedule"]).filter(|value| !value.trim().is_empty())
    {
        apply_cron_schedule_input(&mut job, &schedule)?;
    } else {
        if let Some(kind) = string_arg(payload, &["scheduleKind", "schedule_kind"]) {
            job.schedule_kind = kind.trim().to_lowercase();
        }
        if payload_has_any(payload, &["intervalMinutes", "interval_minutes"]) {
            job.interval_minutes = payload
                .get("intervalMinutes")
                .or_else(|| payload.get("interval_minutes"))
                .and_then(Value::as_u64);
        }
        if payload_has_any(payload, &["cronExpr", "cron_expr"]) {
            job.cron_expr = string_arg(payload, &["cronExpr", "cron_expr"]);
        }
        if payload_has_any(payload, &["runAt", "run_at"]) {
            job.run_at = string_arg(payload, &["runAt", "run_at"]);
        }
    }
    if payload_has_any(payload, &["repeat", "repeatTimes", "repeat_times"]) {
        job.repeat = payload
            .get("repeat")
            .or_else(|| payload.get("repeatTimes"))
            .or_else(|| payload.get("repeat_times"))
            .and_then(Value::as_u64);
    }
    let saved = store.save_scheduled_agent_job(job)?;
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "update",
        "jobId": saved.id,
        "job_id": saved.id,
        "name": saved.name,
        "schedule": saved.schedule_display,
        "nextRunAt": saved.next_run_at,
        "next_run_at": saved.next_run_at,
        "skills": saved.skills,
        "job": saved
    }))
}

fn cronjob_set_enabled(store: &AppStore, payload: &Value, enabled: bool) -> AppResult<Value> {
    let job_id = resolve_cronjob_id(store, payload)?;
    let job = store.set_scheduled_agent_job_enabled(&job_id, enabled)?;
    Ok(json!({
        "ok": true,
        "success": true,
        "action": if enabled { "resume" } else { "pause" },
        "jobId": job.id,
        "job_id": job.id,
        "nextRunAt": job.next_run_at,
        "next_run_at": job.next_run_at,
        "job": job
    }))
}

fn cronjob_delete(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let job_id = resolve_cronjob_id(store, payload)?;
    store.delete_scheduled_agent_job(&job_id)?;
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "delete",
        "jobId": job_id,
        "job_id": job_id
    }))
}

fn cronjob_trigger(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let job_id = resolve_cronjob_id(store, payload)?;
    let job = store.trigger_scheduled_agent_job(&job_id)?;
    Ok(json!({
        "ok": true,
        "success": true,
        "action": "trigger",
        "jobId": job.id,
        "job_id": job.id,
        "nextRunAt": job.next_run_at,
        "next_run_at": job.next_run_at,
        "job": job,
        "started": false,
        "queued": true,
        "note": "Manual trigger queued the job for the next desktop scheduler tick."
    }))
}

fn resolve_cronjob_id(store: &AppStore, payload: &Value) -> AppResult<String> {
    let selector = string_arg(payload, &["jobId", "job_id", "id", "name"])
        .ok_or_else(|| AppError::BadRequest("cronjob action requires jobId or name".into()))?;
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::BadRequest(
            "cronjob action requires non-empty jobId or name".into(),
        ));
    }
    let jobs = store.scheduled_agent_jobs()?;
    if let Some(job) = jobs.iter().find(|job| job.id == selector) {
        return Ok(job.id.clone());
    }
    let matches = jobs
        .iter()
        .filter(|job| job.id.starts_with(selector) || job.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [job] => Ok(job.id.clone()),
        [] => Err(AppError::NotFound(format!(
            "scheduled agent job not found: {selector}"
        ))),
        _ => Err(AppError::BadRequest(format!(
            "cronjob selector '{selector}' matched multiple jobs; use a full jobId"
        ))),
    }
}

fn resolve_cron_persona(store: &AppStore, selector: &str) -> AppResult<Persona> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::BadRequest(
            "cronjob profile/persona selector cannot be empty".into(),
        ));
    }
    let personas = store.personas()?;
    if let Some(persona) = personas.iter().find(|persona| persona.id == selector) {
        return Ok(persona.clone());
    }
    let matches = personas
        .iter()
        .filter(|persona| persona.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [persona] => Ok((*persona).clone()),
        [] => Err(AppError::NotFound(format!(
            "cronjob profile/persona not found: {selector}"
        ))),
        _ => Err(AppError::BadRequest(format!(
            "cronjob profile/persona selector '{selector}' matched multiple personas; use personaId"
        ))),
    }
}

fn resolve_cron_agent_id(store: &AppStore, selector: &str) -> AppResult<String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::BadRequest(
            "cronjob agent selector cannot be empty".into(),
        ));
    }
    let agents = store.agents()?;
    if let Some(agent) = agents.iter().find(|agent| agent.id == selector) {
        return Ok(agent.id.clone());
    }
    let matches = agents
        .iter()
        .filter(|agent| agent.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [agent] => Ok(agent.id.clone()),
        [] => Err(AppError::NotFound(format!(
            "cronjob agent not found: {selector}"
        ))),
        _ => Err(AppError::BadRequest(format!(
            "cronjob agent selector '{selector}' matched multiple agents; use agentId"
        ))),
    }
}

pub(super) fn apply_cron_schedule_input(
    job: &mut ScheduledAgentJob,
    schedule: &str,
) -> AppResult<()> {
    let schedule = schedule.trim();
    if schedule.split_whitespace().count() == 5 {
        job.schedule_kind = "cron".into();
        job.cron_expr = Some(schedule.into());
        job.schedule_display = schedule.into();
        job.run_at = None;
        job.interval_minutes = None;
        return Ok(());
    }
    if let Some(rest) = schedule.strip_prefix("every ") {
        job.schedule_kind = "interval".into();
        job.interval_minutes = Some(parse_duration_minutes(rest)?);
        job.schedule_display = schedule.into();
        job.run_at = None;
        job.cron_expr = None;
        return Ok(());
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(schedule) {
        job.schedule_kind = "once".into();
        job.run_at = Some(timestamp.with_timezone(&Utc).to_rfc3339());
        job.schedule_display = schedule.into();
        job.interval_minutes = None;
        job.cron_expr = None;
        return Ok(());
    }
    let minutes = parse_duration_minutes(schedule)?;
    job.schedule_kind = "once".into();
    job.run_at = Some((Utc::now() + ChronoDuration::minutes(minutes as i64)).to_rfc3339());
    job.schedule_display = schedule.into();
    job.interval_minutes = None;
    job.cron_expr = None;
    Ok(())
}

pub(super) fn parse_duration_minutes(value: &str) -> AppResult<u64> {
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return Err(AppError::BadRequest(
            "schedule duration cannot be empty".into(),
        ));
    }
    let (number, multiplier) = if let Some(number) = value.strip_suffix("min") {
        (number.trim(), 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number.trim(), 1)
    } else if let Some(number) = value
        .strip_suffix("hour")
        .or_else(|| value.strip_suffix("hours"))
    {
        (number.trim(), 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number.trim(), 60)
    } else if let Some(number) = value
        .strip_suffix("day")
        .or_else(|| value.strip_suffix("days"))
    {
        (number.trim(), 60 * 24)
    } else if let Some(number) = value.strip_suffix('d') {
        (number.trim(), 60 * 24)
    } else {
        (value.as_str(), 1)
    };
    let amount = number.parse::<u64>().map_err(|_| {
        AppError::BadRequest(format!(
            "invalid schedule duration '{value}'. Use examples like 30m, 2h, or 1d."
        ))
    })?;
    let minutes = amount.saturating_mul(multiplier);
    if minutes == 0 {
        return Err(AppError::BadRequest(
            "schedule duration must be greater than zero".into(),
        ));
    }
    Ok(minutes)
}

fn string_list_arg(payload: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(text) = value.as_str() {
            return text
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    vec![]
}

fn u64_arg(payload: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        if let Some(number) = value.as_u64() {
            return Some(number);
        }
        if let Some(text) = value.as_str() {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            if let Ok(number) = text.parse::<u64>() {
                return Some(number);
            }
        }
    }
    None
}

fn payload_has_any(payload: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| payload.get(*key).is_some())
}

fn normalize_skill_list(skill: Option<&str>, skills: Option<&Value>) -> Vec<String> {
    let mut normalized = vec![];
    let mut push = |value: &str| {
        let text = value.trim();
        if !text.is_empty() && !normalized.iter().any(|item| item == text) {
            normalized.push(text.to_string());
        }
    };
    if let Some(items) = skills.and_then(Value::as_array) {
        for item in items.iter().filter_map(Value::as_str) {
            push(item);
        }
    } else if let Some(text) = skills.and_then(Value::as_str) {
        for item in text.split(',') {
            push(item);
        }
    } else if let Some(text) = skill {
        push(text);
    }
    normalized
}
