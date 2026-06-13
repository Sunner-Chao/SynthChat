use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{AgentDefinition, ChatMessage, LlmProvider, Persona},
    store::AppStore,
};

use super::{
    complete_chat_with_provider_failover, list_agent_auxiliary_task_assignments,
    selected_provider_id,
};

const PROFILE_DESCRIBER_SYSTEM_PROMPT: &str = r#"You are a profile describer for an agent router.

You receive one agent profile: name, model/provider, enabled toolsets, and enabled skills.
Write exactly one JSON object:
{"description":"<1-2 sentence concrete description, <=280 characters>"}

Rules:
- Describe what this agent is good at so an orchestrator can route work to it.
- Stay concrete and do not invent capabilities.
- Do not mention "SynthChat", "Hermes", "profile", or "agent" unless the name requires it.
- Output JSON only."#;

struct ProfileDescriberPlan {
    providers: Vec<LlmProvider>,
    persona: Persona,
}

pub(crate) async fn auto_describe_agent(
    store: &AppStore,
    agent_id: Option<String>,
    overwrite: bool,
) -> AppResult<AgentDefinition> {
    let mut agent = store.agent(agent_id.as_deref())?;
    if !overwrite && !agent.description.trim().is_empty() {
        return Ok(agent);
    }

    let description = match generate_agent_description_with_auxiliary(store, &agent).await {
        Ok(Some(description)) => description,
        Ok(None) | Err(_) => deterministic_agent_description(store, &agent)?,
    };
    agent.description = description;
    store.save_agent(agent)
}

async fn generate_agent_description_with_auxiliary(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<Option<String>> {
    let Some(plan) = build_profile_describer_plan(store, agent)? else {
        return Ok(None);
    };
    let prompt = profile_describer_user_prompt(store, agent)?;
    let message = ChatMessage::new(
        "__profile_describer__".into(),
        "user",
        prompt.clone(),
        "internal",
    );
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &plan.providers,
        &plan.persona,
        PROFILE_DESCRIBER_SYSTEM_PROMPT.into(),
        vec![message],
        &prompt,
        None,
        None,
    )
    .await?;
    Ok(clean_profile_description(&reply.content))
}

fn build_profile_describer_plan(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<Option<ProfileDescriberPlan>> {
    let Some(assignment) = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == "profile_describer")
    else {
        return Ok(None);
    };
    let provider = assignment.provider.trim();
    let provider_id = if provider.eq_ignore_ascii_case("auto") {
        ""
    } else {
        provider
    };
    let model = assignment.model.trim();
    let base_url = assignment.base_url.trim();
    if provider_id.is_empty() && model.is_empty() && base_url.is_empty() {
        return Ok(None);
    }

    let selected = if provider_id.is_empty() {
        selected_provider_id(&store.persona(None)?, agent).map(str::to_string)
    } else {
        Some(provider_id.to_string())
    };
    let main_providers = store.provider_candidates(selected.as_deref())?;
    let mut providers = if !base_url.is_empty() {
        vec![LlmProvider {
            id: "auxiliary-profile-describer-custom".into(),
            name: "Profile describer auxiliary".into(),
            provider_type: "openai_compatible".into(),
            base_url: base_url.into(),
            append_chat_path: true,
            api_key: (!assignment.api_key.trim().is_empty())
                .then(|| assignment.api_key.trim().to_string()),
            model: if model.is_empty() {
                main_providers
                    .first()
                    .map(|provider| provider.model.clone())
                    .unwrap_or_default()
            } else {
                model.to_string()
            },
            enabled: true,
            timeout_seconds: assignment.timeout,
            ..LlmProvider::default()
        }]
    } else {
        main_providers
    };
    if providers.is_empty() {
        return Err(AppError::NotFound("profile_describer llm provider".into()));
    }
    if !model.is_empty() {
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    for provider in &mut providers {
        provider.timeout_seconds = assignment.timeout;
    }

    let mut persona = store.persona(None)?;
    if let Some(provider_id) = selected.as_deref() {
        persona.llm_provider = provider_id.to_string();
    }
    if !model.is_empty() {
        persona.llm_model = model.to_string();
    }
    persona.temperature = 0.3;
    persona.max_tokens = 400;
    Ok(Some(ProfileDescriberPlan { providers, persona }))
}

fn profile_describer_user_prompt(store: &AppStore, agent: &AgentDefinition) -> AppResult<String> {
    let skill_names = enabled_skill_names(store, agent)?;
    let provider = if agent.llm_provider.trim().is_empty() {
        "(default)"
    } else {
        agent.llm_provider.trim()
    };
    let model = if agent.llm_model.trim().is_empty() {
        "(default)"
    } else {
        agent.llm_model.trim()
    };
    let toolsets = if agent.enabled_toolsets.is_empty() {
        "(default)".to_string()
    } else {
        agent.enabled_toolsets.join(", ")
    };
    Ok(format!(
        "Name: {}\nProvider: {}\nModel: {}\nEnabled toolsets: {}\nEnabled skill count: {}\nNotable skills:\n{}",
        agent.name.trim(),
        provider,
        model,
        toolsets,
        skill_names.len(),
        if skill_names.is_empty() {
            "  (no skills enabled)".into()
        } else {
            skill_names
                .iter()
                .take(60)
                .map(|name| format!("  - {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    ))
}

fn deterministic_agent_description(store: &AppStore, agent: &AgentDefinition) -> AppResult<String> {
    let skills = enabled_skill_names(store, agent)?;
    let mut capabilities = Vec::new();
    if agent.allow_shell {
        capabilities.push("runs shell-assisted coding workflows");
    }
    if agent.mcp_enabled {
        capabilities.push("uses MCP tools");
    }
    if agent.skills_enabled && !skills.is_empty() {
        capabilities.push("applies enabled skills");
    }
    if capabilities.is_empty() {
        capabilities.push("handles general chat and planning tasks");
    }
    let skill_hint = if skills.is_empty() {
        String::new()
    } else {
        format!(
            " Notable skills: {}.",
            skills.into_iter().take(6).collect::<Vec<_>>().join(", ")
        )
    };
    Ok(truncate_description(&format!(
        "{} {}.{}",
        agent.name.trim(),
        capabilities.join(", "),
        skill_hint
    )))
}

fn enabled_skill_names(store: &AppStore, agent: &AgentDefinition) -> AppResult<Vec<String>> {
    let mut skills = crate::skills::list_skills_for_agent(store, &agent.id)?
        .into_iter()
        .filter(|skill| skill.enabled)
        .map(|skill| {
            if skill.id.trim().is_empty() {
                skill.name
            } else {
                skill.id
            }
        })
        .collect::<Vec<_>>();
    skills.sort();
    Ok(skills)
}

fn clean_profile_description(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = extract_profile_description_json(trimmed).and_then(|value| {
        value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let text = parsed.unwrap_or_else(|| {
        trimmed
            .trim_matches('`')
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    });
    let text = text.trim().trim_matches('"').trim();
    if text.is_empty() {
        None
    } else {
        Some(truncate_description(text))
    }
}

fn extract_profile_description_json(raw: &str) -> Option<Value> {
    let first = raw.find('{')?;
    let last = raw.rfind('}')?;
    if last <= first {
        return None;
    }
    serde_json::from_str(&raw[first..=last]).ok()
}

fn truncate_description(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 280 {
        text
    } else {
        text.chars().take(280).collect()
    }
}

#[allow(dead_code)]
fn _profile_describer_prompt_preview(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<Value> {
    Ok(json!({
        "system": PROFILE_DESCRIBER_SYSTEM_PROMPT,
        "user": profile_describer_user_prompt(store, agent)?,
    }))
}
