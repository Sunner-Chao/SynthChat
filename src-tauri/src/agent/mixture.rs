use std::time::Instant;

use futures::future::join_all;
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{LlmProvider, Persona},
    store::AppStore,
};

use super::{
    append_parent_phase_event, complete_chat_with_provider_failover, string_arg, string_list_arg,
    truncate_output,
};

pub(super) async fn mixture_of_agents_tool(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let user_prompt = string_arg(payload, &["user_prompt", "userPrompt", "prompt", "task"])
        .ok_or_else(|| {
            AppError::BadRequest("mixture_of_agents requires payload.user_prompt".into())
        })?;
    let parent = store.agent_run(run_id)?;
    let persona = store.persona(Some(&parent.persona_id))?;
    let reference_count = payload
        .get("referenceCount")
        .or_else(|| payload.get("reference_count"))
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 8) as usize;
    let min_successful = payload
        .get("minSuccessfulReferences")
        .or_else(|| payload.get("min_successful_references"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, reference_count as u64) as usize;
    let started = Instant::now();
    let reference_providers =
        mixture_reference_providers(store, &persona, payload, reference_count)?;
    let aggregator_provider = mixture_aggregator_provider(store, &persona, payload)?;
    let aggregator_providers = store.provider_candidates(Some(&aggregator_provider.id))?;
    let history = store.messages(conversation_id, Some(8))?;
    let reference_provider_chains = reference_providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            Ok((
                index,
                provider.id.clone(),
                store.provider_candidates(Some(&provider.id))?,
            ))
        })
        .collect::<AppResult<Vec<_>>>()?;

    let reference_tasks = reference_provider_chains
        .into_iter()
        .map(|(index, requested_provider_id, providers)| {
            let mut persona = persona.clone();
            persona.temperature = payload
                .get("referenceTemperature")
                .or_else(|| payload.get("reference_temperature"))
                .and_then(Value::as_f64)
                .unwrap_or(0.6)
                .clamp(0.0, 2.0) as f32;
            let history = history.clone();
            let user_prompt = user_prompt.clone();
            let store = store;
            let run_id = run_id.to_string();
            async move {
                let system_prompt = mixture_reference_system_prompt(index + 1);
                let result = complete_chat_with_provider_failover(
                    store,
                    Some(&run_id),
                    &providers,
                    &persona,
                    system_prompt,
                    history,
                    &user_prompt,
                    None,
                    None,
                )
                .await;
                (index, requested_provider_id, result)
            }
        })
        .collect::<Vec<_>>();
    let reference_results = join_all(reference_tasks).await;
    let mut successful = Vec::new();
    let mut failed = Vec::new();
    for (index, provider_id, result) in reference_results {
        match result {
            Ok(reply) if !reply.content.trim().is_empty() => {
                let final_provider_id = reply
                    .provider_id
                    .clone()
                    .unwrap_or_else(|| provider_id.clone());
                let failover_attempts = reply
                    .failover_attempts
                    .iter()
                    .map(|attempt| {
                        json!({
                            "providerId": attempt.provider_id,
                            "kind": attempt.kind,
                            "message": attempt.message,
                        })
                    })
                    .collect::<Vec<_>>();
                successful.push(json!({
                    "index": index + 1,
                    "requestedProviderId": provider_id,
                    "providerId": final_provider_id,
                    "model": reply.model.clone(),
                    "failoverAttempts": failover_attempts,
                    "content": reply.content,
                }));
            }
            Ok(_) => failed.push(json!({
                "index": index + 1,
                "providerId": provider_id,
                "error": "empty model response",
            })),
            Err(error) => failed.push(json!({
                "index": index + 1,
                "providerId": provider_id,
                "error": error.to_string(),
            })),
        }
    }
    if successful.len() < min_successful {
        return Err(AppError::BadRequest(format!(
            "mixture_of_agents had insufficient successful references ({}/{}), need at least {}",
            successful.len(),
            reference_count,
            min_successful
        )));
    }

    let reference_texts = successful
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut aggregator_persona = persona.clone();
    aggregator_persona.temperature = payload
        .get("aggregatorTemperature")
        .or_else(|| payload.get("aggregator_temperature"))
        .and_then(Value::as_f64)
        .unwrap_or(0.4)
        .clamp(0.0, 2.0) as f32;
    let aggregator_system_prompt = mixture_aggregator_system_prompt(&reference_texts);
    let final_reply = complete_chat_with_provider_failover(
        store,
        Some(run_id),
        &aggregator_providers,
        &aggregator_persona,
        aggregator_system_prompt,
        history,
        &user_prompt,
        None,
        None,
    )
    .await?;
    let result = json!({
        "success": true,
        "response": final_reply.content,
        "modelsUsed": {
            "referenceProviderIds": reference_providers.iter().map(|provider| provider.id.clone()).collect::<Vec<_>>(),
            "aggregatorProviderId": final_reply.provider_id.clone().unwrap_or(aggregator_provider.id.clone()),
        },
        "referenceResponses": successful,
        "failedReferences": failed,
        "processingTimeMs": started.elapsed().as_millis(),
    });
    append_parent_phase_event(
        store,
        run_id,
        "mixture_of_agents_completed",
        json!({
            "referenceCount": reference_count,
            "successfulReferences": result["referenceResponses"].as_array().map(Vec::len).unwrap_or(0),
            "failedReferences": result["failedReferences"].as_array().map(Vec::len).unwrap_or(0),
            "aggregatorProviderId": result["modelsUsed"]["aggregatorProviderId"].clone(),
        }),
    )?;
    Ok(serde_json::to_string_pretty(&result)?)
}

pub(super) fn mixture_reference_providers(
    store: &AppStore,
    persona: &Persona,
    payload: &Value,
    reference_count: usize,
) -> AppResult<Vec<LlmProvider>> {
    let requested = string_list_arg(
        payload,
        &[
            "referenceProviderIds",
            "reference_provider_ids",
            "referenceModels",
            "reference_models",
        ],
    );
    let mut providers = Vec::new();
    for id in requested {
        providers.push(store.provider(Some(&id))?);
    }
    if providers.is_empty() {
        providers = store.provider_candidates(Some(&persona.llm_provider))?;
    }
    if providers.is_empty() {
        return Err(AppError::NotFound("llm provider".into()));
    }
    let mut expanded = Vec::new();
    while expanded.len() < reference_count {
        for provider in &providers {
            expanded.push(provider.clone());
            if expanded.len() >= reference_count {
                break;
            }
        }
    }
    Ok(expanded)
}

fn mixture_aggregator_provider(
    store: &AppStore,
    persona: &Persona,
    payload: &Value,
) -> AppResult<LlmProvider> {
    if let Some(id) = string_arg(
        payload,
        &[
            "aggregatorProviderId",
            "aggregator_provider_id",
            "aggregatorModel",
            "aggregator_model",
        ],
    ) {
        return store.provider(Some(&id));
    }
    store
        .provider(Some(&persona.llm_provider))
        .or_else(|_| store.provider(None))
}

pub(super) fn mixture_reference_system_prompt(index: usize) -> String {
    format!(
        "You are reference agent #{index} in a Mixture-of-Agents workflow. Solve the user's hard problem independently. Favor rigorous reasoning, surface assumptions, and do not mention other agents."
    )
}

pub(super) fn mixture_aggregator_system_prompt(reference_responses: &[String]) -> String {
    let mut prompt = String::from(
        "You are the aggregator in a Mixture-of-Agents workflow. Synthesize the strongest answer from the independent reference responses. Resolve conflicts, keep correct details, discard weak reasoning, and return a final answer directly to the user.\n\nReference responses:\n",
    );
    for (index, response) in reference_responses.iter().enumerate() {
        prompt.push_str(&format!(
            "\n[Reference {}]\n{}\n",
            index + 1,
            truncate_output(response, 8000)
        ));
    }
    prompt
}
