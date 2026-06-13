use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{now_iso, AgentGoalState},
    store::AppStore,
};

const GOAL_METADATA_KEY: &str = "agentGoal";
const DEFAULT_MAX_TURNS: u32 = 20;
const MAX_CONSECUTIVE_PARSE_FAILURES: u32 = 3;

const CONTINUATION_PROMPT_TEMPLATE: &str = "[Continuing toward your standing goal]\nGoal: {goal}\n\nContinue working toward this goal. Take the next concrete step. If you believe the goal is complete, state so explicitly and stop. If you are blocked and need input from the user, say so clearly and stop.";

const CONTINUATION_PROMPT_WITH_SUBGOALS_TEMPLATE: &str = "[Continuing toward your standing goal]\nGoal: {goal}\n\nAdditional criteria the user added mid-loop:\n{subgoals}\n\nContinue working toward the goal AND all additional criteria. Take the next concrete step. If you believe the goal and every additional criterion are complete, state so explicitly and stop. If you are blocked and need input from the user, say so clearly and stop.";

pub(super) fn agent_goal_status(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentGoalState>> {
    load_agent_goal(store, conversation_id)
}

pub(super) fn set_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    goal: &str,
    max_turns: Option<u32>,
) -> AppResult<AgentGoalState> {
    store.conversation(conversation_id)?;
    let goal = goal.trim();
    if goal.is_empty() {
        return Err(AppError::BadRequest("goal text is empty".into()));
    }
    let state = AgentGoalState {
        goal: goal.into(),
        status: "active".into(),
        turns_used: 0,
        max_turns: max_turns.unwrap_or(DEFAULT_MAX_TURNS).max(1),
        created_at: now_iso(),
        last_turn_at: None,
        last_verdict: None,
        last_reason: None,
        paused_reason: None,
        consecutive_parse_failures: 0,
        subgoals: Vec::new(),
    };
    save_agent_goal(store, conversation_id, &state)
}

pub(super) fn pause_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    reason: Option<&str>,
) -> AppResult<Option<AgentGoalState>> {
    mutate_agent_goal(store, conversation_id, |state| {
        state.status = "paused".into();
        state.paused_reason = reason
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| Some("user-paused".into()));
    })
}

pub(super) fn resume_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    reset_budget: bool,
) -> AppResult<Option<AgentGoalState>> {
    mutate_agent_goal(store, conversation_id, |state| {
        state.status = "active".into();
        state.paused_reason = None;
        if reset_budget {
            state.turns_used = 0;
            state.consecutive_parse_failures = 0;
        }
    })
}

pub(super) fn clear_agent_goal(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentGoalState>> {
    mutate_agent_goal(store, conversation_id, |state| {
        state.status = "cleared".into();
    })
}

pub(super) fn add_agent_subgoal(
    store: &AppStore,
    conversation_id: &str,
    text: &str,
) -> AppResult<Option<AgentGoalState>> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::BadRequest("subgoal text is empty".into()));
    }
    mutate_agent_goal(store, conversation_id, |state| {
        state.subgoals.push(text.into());
    })
}

pub(super) fn remove_agent_subgoal(
    store: &AppStore,
    conversation_id: &str,
    index: usize,
) -> AppResult<Option<AgentGoalState>> {
    let Some(mut state) = load_agent_goal(store, conversation_id)? else {
        return Ok(None);
    };
    if index == 0 || index > state.subgoals.len() {
        return Err(AppError::BadRequest(format!(
            "subgoal index out of range: {index}"
        )));
    }
    state.subgoals.remove(index - 1);
    state.last_turn_at.get_or_insert_with(now_iso);
    save_agent_goal(store, conversation_id, &state).map(Some)
}

pub(super) fn clear_agent_subgoals(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentGoalState>> {
    mutate_agent_goal(store, conversation_id, |state| {
        state.subgoals.clear();
    })
}

pub(super) fn agent_goal_continuation_prompt(state: &AgentGoalState) -> Option<String> {
    if state.status != "active" {
        return None;
    }
    if state.subgoals.is_empty() {
        Some(CONTINUATION_PROMPT_TEMPLATE.replace("{goal}", &state.goal))
    } else {
        Some(
            CONTINUATION_PROMPT_WITH_SUBGOALS_TEMPLATE
                .replace("{goal}", &state.goal)
                .replace("{subgoals}", &render_subgoals(&state.subgoals)),
        )
    }
}

pub(super) fn record_agent_goal_verdict(
    store: &AppStore,
    conversation_id: &str,
    done: bool,
    reason: &str,
    parse_failed: bool,
) -> AppResult<Option<AgentGoalState>> {
    let Some(mut state) = load_agent_goal(store, conversation_id)? else {
        return Ok(None);
    };
    if state.status != "active" {
        return Ok(Some(state));
    }
    state.turns_used = state.turns_used.saturating_add(1);
    state.last_turn_at = Some(now_iso());
    state.last_verdict = Some(if done { "done" } else { "continue" }.into());
    state.last_reason = Some(reason.trim().to_string());
    if parse_failed {
        state.consecutive_parse_failures = state.consecutive_parse_failures.saturating_add(1);
    } else {
        state.consecutive_parse_failures = 0;
    }
    if done {
        state.status = "done".into();
    } else if state.consecutive_parse_failures >= MAX_CONSECUTIVE_PARSE_FAILURES {
        state.status = "paused".into();
        state.paused_reason = Some(format!(
            "judge returned unparseable output {} turns in a row",
            state.consecutive_parse_failures
        ));
    } else if state.turns_used >= state.max_turns {
        state.status = "paused".into();
        state.paused_reason = Some(format!(
            "turn budget exhausted ({}/{})",
            state.turns_used, state.max_turns
        ));
    }
    save_agent_goal(store, conversation_id, &state).map(Some)
}

pub(super) fn pause_agent_goal_for_preempting_queue(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentGoalState>> {
    pause_agent_goal(
        store,
        conversation_id,
        Some("pending user queue item preempted goal continuation"),
    )
}

fn load_agent_goal(store: &AppStore, conversation_id: &str) -> AppResult<Option<AgentGoalState>> {
    let conversation = store.conversation(conversation_id)?;
    Ok(conversation
        .metadata
        .get(GOAL_METADATA_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value::<AgentGoalState>(value).ok())
        .filter(|state| state.status != "cleared"))
}

fn save_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    state: &AgentGoalState,
) -> AppResult<AgentGoalState> {
    store.set_conversation_metadata_value(conversation_id, GOAL_METADATA_KEY, json!(state))?;
    Ok(state.clone())
}

fn mutate_agent_goal<F>(
    store: &AppStore,
    conversation_id: &str,
    mutate: F,
) -> AppResult<Option<AgentGoalState>>
where
    F: FnOnce(&mut AgentGoalState),
{
    let Some(mut state) = load_agent_goal(store, conversation_id)? else {
        return Ok(None);
    };
    mutate(&mut state);
    state.last_turn_at.get_or_insert_with(now_iso);
    save_agent_goal(store, conversation_id, &state).map(Some)
}

fn render_subgoals(subgoals: &[String]) -> String {
    subgoals
        .iter()
        .enumerate()
        .map(|(index, value)| format!("- {}. {}", index + 1, value))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn agent_goal_to_json(state: Option<AgentGoalState>) -> Value {
    match state {
        Some(state) => {
            let continuation_prompt = agent_goal_continuation_prompt(&state);
            json!({
                "ok": true,
                "goal": state,
                "continuationPrompt": continuation_prompt,
            })
        }
        None => json!({"ok": true, "goal": null, "continuationPrompt": null}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::new_id;

    #[test]
    fn agent_goal_state_persists_in_conversation_metadata() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-state-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let conversation = store
            .create_conversation(Some("Goal".into()), Some("default".into()))
            .unwrap();

        let state = set_agent_goal(&store, &conversation.id, "Ship Hermes parity", Some(3))
            .expect("goal should be saved");
        assert_eq!(state.status, "active");
        assert_eq!(state.max_turns, 3);
        assert_eq!(
            agent_goal_status(&store, &conversation.id)
                .unwrap()
                .unwrap()
                .goal,
            "Ship Hermes parity"
        );

        let with_subgoal = add_agent_subgoal(&store, &conversation.id, "prove tests").unwrap();
        assert_eq!(with_subgoal.unwrap().subgoals, vec!["prove tests"]);
        let prompt = agent_goal_continuation_prompt(
            &agent_goal_status(&store, &conversation.id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(prompt.contains("Ship Hermes parity"));
        assert!(prompt.contains("prove tests"));

        let paused = pause_agent_goal(&store, &conversation.id, Some("manual")).unwrap();
        assert_eq!(paused.unwrap().status, "paused");
        let resumed = resume_agent_goal(&store, &conversation.id, true).unwrap();
        assert_eq!(resumed.unwrap().status, "active");
        let cleared = clear_agent_goal(&store, &conversation.id).unwrap();
        assert_eq!(cleared.unwrap().status, "cleared");
        assert!(agent_goal_status(&store, &conversation.id)
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_goal_verdict_updates_budget_and_parse_failure_state() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-verdict-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let conversation = store
            .create_conversation(Some("Goal".into()), Some("default".into()))
            .unwrap();

        set_agent_goal(&store, &conversation.id, "Finish task", Some(2)).unwrap();
        let continued = record_agent_goal_verdict(
            &store,
            &conversation.id,
            false,
            "not enough evidence",
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(continued.status, "active");
        assert_eq!(continued.turns_used, 1);
        assert_eq!(continued.last_verdict.as_deref(), Some("continue"));

        let paused =
            record_agent_goal_verdict(&store, &conversation.id, false, "still not done", false)
                .unwrap()
                .unwrap();
        assert_eq!(paused.status, "paused");
        assert!(paused
            .paused_reason
            .as_deref()
            .unwrap_or_default()
            .contains("turn budget exhausted"));

        set_agent_goal(&store, &conversation.id, "Judge parse", Some(10)).unwrap();
        for _ in 0..3 {
            record_agent_goal_verdict(&store, &conversation.id, false, "bad json", true).unwrap();
        }
        let parse_paused = agent_goal_status(&store, &conversation.id)
            .unwrap()
            .unwrap();
        assert_eq!(parse_paused.status, "paused");
        assert_eq!(parse_paused.consecutive_parse_failures, 3);

        let _ = std::fs::remove_dir_all(dir);
    }
}
