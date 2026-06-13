use serde_json::Value;

use crate::{
    error::AppResult,
    models::{ChatMessage, LlmProvider, Persona},
    store::AppStore,
};

use super::{complete_chat_with_provider_failover, list_agent_auxiliary_task_assignments};

const GOAL_JUDGE_SYSTEM_PROMPT: &str = "You are a strict judge evaluating whether an autonomous agent has achieved a user's stated goal. A goal is done only when the response explicitly confirms completion, clearly shows the deliverable was produced, or explains the goal is blocked and needs user input. Reply only with one JSON object: {\"done\":true|false,\"reason\":\"one sentence\"}.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GoalJudgeVerdict {
    pub done: bool,
    pub reason: String,
    pub parse_failed: bool,
    pub model: String,
}

pub(super) async fn judge_goal_completion(
    store: &AppStore,
    goal: &str,
    response: &str,
    subgoals: &[String],
    main_providers: &[LlmProvider],
    main_persona: &Persona,
) -> AppResult<GoalJudgeVerdict> {
    if goal.trim().is_empty() {
        return Ok(GoalJudgeVerdict {
            done: false,
            reason: "empty goal".into(),
            parse_failed: false,
            model: String::new(),
        });
    }
    if response.trim().is_empty() {
        return Ok(GoalJudgeVerdict {
            done: false,
            reason: "empty response".into(),
            parse_failed: false,
            model: String::new(),
        });
    }
    let (providers, persona, model_label) =
        goal_judge_provider_plan(store, main_providers, main_persona)?;
    if providers.is_empty() {
        let mut verdict = deterministic_goal_judge(response);
        verdict.model = "deterministic".into();
        return Ok(verdict);
    }
    let user_prompt = goal_judge_user_prompt(goal, response, subgoals);
    let history = vec![ChatMessage::new(
        "__goal_judge__".into(),
        "user",
        user_prompt.clone(),
        "goal_judge",
    )];
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &providers,
        &persona,
        GOAL_JUDGE_SYSTEM_PROMPT.to_string(),
        history,
        &user_prompt,
        None,
        None,
    )
    .await?;
    let mut verdict = parse_goal_judge_response(&reply.content);
    verdict.model = model_label;
    Ok(verdict)
}

fn goal_judge_provider_plan(
    store: &AppStore,
    main_providers: &[LlmProvider],
    main_persona: &Persona,
) -> AppResult<(Vec<LlmProvider>, Persona, String)> {
    let Some(assignment) = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == "goal_judge")
    else {
        return Ok((main_providers.to_vec(), main_persona.clone(), String::new()));
    };
    let provider = assignment.provider.trim();
    let provider_id = if provider.eq_ignore_ascii_case("auto") {
        ""
    } else {
        provider
    };
    let model = assignment.model.trim();
    let base_url = assignment.base_url.trim();
    let mut providers = if !base_url.is_empty() {
        vec![LlmProvider {
            id: "auxiliary-goal-judge-custom".into(),
            name: "Goal judge auxiliary".into(),
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
            timeout_seconds: assignment.timeout.max(1),
            ..LlmProvider::default()
        }]
    } else if provider_id.is_empty() {
        main_providers.to_vec()
    } else {
        store.provider_candidates(Some(provider_id))?
    };
    if !model.is_empty() {
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    for provider in &mut providers {
        provider.timeout_seconds = assignment.timeout.max(1);
    }
    let mut persona = main_persona.clone();
    persona.temperature = 0.0;
    persona.max_tokens = 4_096;
    if !provider_id.is_empty() {
        persona.llm_provider = provider_id.to_string();
    }
    if !model.is_empty() {
        persona.llm_model = model.to_string();
    }
    let label = if model.is_empty() {
        providers
            .first()
            .map(|provider| provider.model.clone())
            .unwrap_or_default()
    } else {
        model.to_string()
    };
    Ok((providers, persona, label))
}

fn goal_judge_user_prompt(goal: &str, response: &str, subgoals: &[String]) -> String {
    let subgoals = subgoals
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .enumerate()
        .map(|(index, value)| format!("{}. {value}", index + 1))
        .collect::<Vec<_>>();
    if subgoals.is_empty() {
        format!(
            "Goal:\n{}\n\nAgent's most recent response:\n{}\n\nIs the goal satisfied?",
            truncate(goal, 2_000),
            truncate(response, 4_000)
        )
    } else {
        format!(
            "Goal:\n{}\n\nAdditional criteria:\n{}\n\nAgent's most recent response:\n{}\n\nIs the goal and every criterion satisfied?",
            truncate(goal, 2_000),
            truncate(&subgoals.join("\n"), 2_000),
            truncate(response, 4_000)
        )
    }
}

pub(super) fn parse_goal_judge_response(raw: &str) -> GoalJudgeVerdict {
    let Some(value) = parse_json_object_from_text(raw) else {
        return GoalJudgeVerdict {
            done: false,
            reason: format!("judge reply was not JSON: {}", truncate(raw, 200)),
            parse_failed: true,
            model: String::new(),
        };
    };
    let done = value
        .get("done")
        .and_then(json_bool)
        .or_else(|| value.get("verdict").and_then(json_done_verdict))
        .unwrap_or(false);
    let reason = value
        .get("reason")
        .or_else(|| value.get("rationale"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("no reason provided")
        .to_string();
    GoalJudgeVerdict {
        done,
        reason,
        parse_failed: false,
        model: String::new(),
    }
}

fn deterministic_goal_judge(response: &str) -> GoalJudgeVerdict {
    let lower = response.to_lowercase();
    let done = [
        "goal achieved",
        "goal complete",
        "completed",
        "done",
        "blocked",
        "need user input",
        "cannot proceed",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    GoalJudgeVerdict {
        done,
        reason: if done {
            "response indicates completion or a blocking condition".into()
        } else {
            "response does not clearly prove the goal is complete".into()
        },
        parse_failed: false,
        model: String::new(),
    }
}

fn parse_json_object_from_text(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok().or_else(|| {
        let start = text.find('{')?;
        let end = text.rfind('}')?;
        serde_json::from_str::<Value>(&text[start..=end]).ok()
    })
}

fn json_bool(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        let text = value.as_str()?.trim().to_lowercase();
        Some(matches!(text.as_str(), "true" | "yes" | "done" | "1"))
    })
}

fn json_done_verdict(value: &Value) -> Option<bool> {
    let text = value.as_str()?.trim().to_lowercase();
    Some(matches!(text.as_str(), "done" | "complete" | "completed"))
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    format!(
        "{}... [truncated]",
        value.chars().take(limit).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::save_agent_auxiliary_task_assignment, models::new_id};

    #[test]
    fn parse_goal_judge_response_accepts_json_and_fences() {
        let verdict = parse_goal_judge_response(
            "```json\n{\"done\":\"true\",\"reason\":\"deliverable exists\"}\n```",
        );
        assert!(verdict.done);
        assert_eq!(verdict.reason, "deliverable exists");
        assert!(!verdict.parse_failed);

        let failed = parse_goal_judge_response("continue please");
        assert!(!failed.done);
        assert!(failed.parse_failed);
    }

    #[test]
    fn goal_judge_provider_plan_uses_custom_auxiliary_assignment() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-judge-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        save_agent_auxiliary_task_assignment(
            &store,
            "goal_judge",
            "auto",
            "judge-model",
            "https://judge.example/v1",
            "secret",
            Some(11),
            None,
        )
        .unwrap();
        let persona = store.persona(None).unwrap();
        let (providers, routed_persona, label) =
            goal_judge_provider_plan(&store, &[LlmProvider::default()], &persona).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "auxiliary-goal-judge-custom");
        assert_eq!(providers[0].base_url, "https://judge.example/v1");
        assert_eq!(providers[0].model, "judge-model");
        assert_eq!(providers[0].timeout_seconds, 11);
        assert_eq!(providers[0].api_key.as_deref(), Some("secret"));
        assert_eq!(routed_persona.llm_model, "judge-model");
        assert_eq!(label, "judge-model");

        let _ = std::fs::remove_dir_all(dir);
    }
}
