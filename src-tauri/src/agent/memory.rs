use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, Conversation, MemoryEntry, Persona},
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{on_memory_write, string_arg};
fn persona_for_conversation(store: &AppStore, conversation_id: &str) -> AppResult<Persona> {
    let conversation = store.conversation(conversation_id)?;
    persona_for_conversation_record(store, &conversation)
}

fn persona_for_conversation_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
) -> AppResult<Persona> {
    let run_id = run_id.trim();
    if !run_id.is_empty() {
        if let Ok(run) = store.agent_run(run_id) {
            let run_persona = store.persona(Some(&run.persona_id)).ok();
            if let Some(persona) = run_persona.as_ref() {
                if persona.agent_id == run.agent_id {
                    return Ok(persona.clone());
                }
            }
            if let Some(persona) = store
                .personas()?
                .into_iter()
                .find(|persona| persona.agent_id == run.agent_id)
                .or(run_persona)
            {
                return Ok(persona);
            }
        }
    }
    let conversation = store.conversation(conversation_id)?;
    persona_for_conversation_record(store, &conversation)
}

fn persona_for_conversation_record(
    store: &AppStore,
    conversation: &Conversation,
) -> AppResult<Persona> {
    let conversation_persona = store.persona(conversation.persona_id.as_deref()).ok();
    if let Some(persona) = conversation_persona.as_ref() {
        if persona.agent_id == conversation.agent_id {
            return Ok(persona.clone());
        }
    }
    store
        .personas()?
        .into_iter()
        .find(|persona| persona.agent_id == conversation.agent_id)
        .or(conversation_persona)
        .map(Ok)
        .unwrap_or_else(|| store.persona(None))
}

pub(super) fn recall_memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    recall_memory_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn recall_memory_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation_run(store, conversation_id, run_id)?;
    let mut payload = payload.clone();
    if payload.get("action").is_none() {
        if let Value::Object(map) = &mut payload {
            map.insert("action".into(), Value::String("read".into()));
        }
    }
    let (text, raw, ok) = execute_manage_memory(store, &persona, &payload)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "recall_memory",
        "text": text,
        "result": raw
    }))?)
}

pub(super) fn remember_fact_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    remember_fact_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn remember_fact_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation_run(store, conversation_id, run_id)?;
    let summary = string_arg(payload, &["summary", "content", "fact"])
        .ok_or_else(|| AppError::BadRequest("remember_fact requires payload.summary".into()))?;
    if summary.trim().is_empty() {
        return Err(AppError::BadRequest(
            "remember_fact summary cannot be empty".into(),
        ));
    }
    let importance = payload
        .get("importance")
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 5) as u8;
    let target = memory_target_from_payload(payload)?;
    let memory = store.save_memory(MemoryEntry {
        id: String::new(),
        persona_id: persona.id.clone(),
        target: target.clone(),
        summary: summary.trim().to_string(),
        importance,
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    on_memory_write(store, run_id, &persona, "add", &memory.id, summary.trim())?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": "remember_fact",
        "target": target,
        "memory": memory
    }))?)
}

pub(super) fn manage_memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    manage_memory_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn manage_memory_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation_run(store, conversation_id, run_id)?;
    let (text, raw, ok) = execute_manage_memory_for_run(store, &persona, run_id, payload)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "manage_memory",
        "text": text,
        "result": raw
    }))?)
}

pub(super) fn memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    memory_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn memory_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let normalized = normalize_memory_payload(payload);
    let persona = persona_for_conversation_run(store, conversation_id, run_id)?;
    let (text, raw, ok) = execute_manage_memory_for_run(store, &persona, run_id, &normalized)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "memory",
        "action": normalized.get("action").and_then(Value::as_str).unwrap_or("read"),
        "text": text,
        "result": raw
    }))?)
}

pub(super) fn memory_provider_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = string_arg(payload, &["action", "command"])
        .unwrap_or_else(|| "status".into())
        .trim()
        .to_ascii_lowercase();
    let active = active_memory_provider();
    let providers = hermes_memory_providers(store);
    let configured = providers
        .iter()
        .find(|provider| provider["name"].as_str() == Some(active.as_str()))
        .cloned()
        .unwrap_or_else(|| json!({"name": active, "available": false, "source": "configured"}));
    let response = match action.as_str() {
        "" | "status" | "active" => json!({
            "ok": true,
            "activeProvider": active,
            "active": configured,
            "providers": providers,
            "hermesMemoryProviderDesktop": memory_provider_boundary(store),
        }),
        "discover" | "list" => json!({
            "ok": true,
            "providers": providers,
            "hermesMemoryProviderDesktop": memory_provider_boundary(store),
        }),
        "tools" => json!({
            "ok": true,
            "activeProvider": active,
            "toolNames": hermes_memory_provider_tool_names(),
            "hermesMemoryProviderDesktop": memory_provider_boundary(store),
        }),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported memory_provider action: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&response)?)
}

pub(super) fn fact_store_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation_run(store, conversation_id, run_id)?;
    let action = string_arg(payload, &["action"])
        .unwrap_or_else(|| "list".into())
        .trim()
        .to_ascii_lowercase();
    let mut facts = load_holographic_facts(store)?;
    let response = match action.as_str() {
        "add" => {
            let content = string_arg(payload, &["content", "summary", "fact"])
                .ok_or_else(|| AppError::BadRequest("fact_store add requires content".into()))?;
            if content.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "fact_store add content cannot be empty".into(),
                ));
            }
            let category = string_arg(payload, &["category"]).unwrap_or_else(|| "general".into());
            let tags = parse_fact_tags(payload);
            let entities = extract_fact_entities(&content);
            let now = now_iso();
            let fact = json!({
                "id": new_id("fact"),
                "content": content.trim(),
                "category": normalize_fact_category(&category),
                "tags": tags,
                "entities": entities,
                "trust": payload.get("trust").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.0, 1.0),
                "createdAt": now,
                "updatedAt": now,
                "source": "synthchat_holographic_desktop"
            });
            facts.push(fact.clone());
            save_holographic_facts(store, &facts)?;
            on_memory_write(
                store,
                run_id,
                &persona,
                "add",
                "holographic",
                content.trim(),
            )?;
            json!({"ok": true, "status": "added", "fact": fact, "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "search" => {
            let query = string_arg(payload, &["query", "q"]).unwrap_or_default();
            let limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let results = rank_holographic_facts(&facts, &query, payload)
                .into_iter()
                .take(limit)
                .collect::<Vec<_>>();
            json!({"ok": true, "results": results, "count": results.len(), "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "probe" | "related" => {
            let entity = string_arg(payload, &["entity"]).ok_or_else(|| {
                AppError::BadRequest(format!("fact_store {action} requires entity"))
            })?;
            let limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let entity_lc = entity.to_ascii_lowercase();
            let results = facts
                .iter()
                .filter(|fact| fact_matches_entity(fact, &entity_lc))
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            json!({"ok": true, "results": results, "count": results.len(), "entity": entity, "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "reason" => {
            let entities = payload
                .get("entities")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|item| item.trim().to_ascii_lowercase())
                        .filter(|item| !item.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if entities.is_empty() {
                return Err(AppError::BadRequest(
                    "fact_store reason requires entities".into(),
                ));
            }
            let limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let results = facts
                .iter()
                .filter(|fact| {
                    entities
                        .iter()
                        .all(|entity| fact_matches_entity(fact, entity))
                })
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            json!({"ok": true, "results": results, "count": results.len(), "entities": entities, "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "contradict" => {
            let contradictions = holographic_contradictions(&facts);
            json!({"ok": true, "results": contradictions, "count": contradictions.len(), "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "update" => {
            let id = string_arg(payload, &["fact_id", "factId", "id"])
                .ok_or_else(|| AppError::BadRequest("fact_store update requires fact_id".into()))?;
            let mut updated = None;
            for fact in &mut facts {
                if fact["id"].as_str() == Some(id.as_str()) {
                    if let Some(content) = string_arg(payload, &["content", "summary", "fact"]) {
                        fact["content"] = json!(content.trim());
                        fact["entities"] = json!(extract_fact_entities(&content));
                    }
                    if let Some(category) = string_arg(payload, &["category"]) {
                        fact["category"] = json!(normalize_fact_category(&category));
                    }
                    if let Some(delta) = payload.get("trust_delta").and_then(Value::as_f64) {
                        let current = fact["trust"].as_f64().unwrap_or(0.5);
                        fact["trust"] = json!((current + delta).clamp(0.0, 1.0));
                    }
                    fact["updatedAt"] = json!(now_iso());
                    updated = Some(fact.clone());
                    break;
                }
            }
            let Some(updated) = updated else {
                return Err(AppError::BadRequest(format!("fact not found: {id}")));
            };
            save_holographic_facts(store, &facts)?;
            json!({"ok": true, "updated": true, "fact": updated, "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "remove" | "delete" => {
            let id = string_arg(payload, &["fact_id", "factId", "id"])
                .ok_or_else(|| AppError::BadRequest("fact_store remove requires fact_id".into()))?;
            let before = facts.len();
            facts.retain(|fact| fact["id"].as_str() != Some(id.as_str()));
            let removed = before != facts.len();
            save_holographic_facts(store, &facts)?;
            json!({"ok": true, "removed": removed, "id": id, "hermesHolographicDesktop": holographic_boundary(store)})
        }
        "list" | "" => {
            let limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 100) as usize;
            let mut listed = facts.clone();
            listed.sort_by(|left, right| {
                right["updatedAt"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(left["updatedAt"].as_str().unwrap_or(""))
            });
            listed.truncate(limit);
            json!({"ok": true, "facts": listed, "count": listed.len(), "total": facts.len(), "hermesHolographicDesktop": holographic_boundary(store)})
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported fact_store action: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&response)?)
}

pub(super) fn fact_feedback_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = string_arg(payload, &["action"])
        .unwrap_or_else(|| "helpful".into())
        .trim()
        .to_ascii_lowercase();
    let id = string_arg(payload, &["fact_id", "factId", "id"])
        .ok_or_else(|| AppError::BadRequest("fact_feedback requires fact_id".into()))?;
    let mut facts = load_holographic_facts(store)?;
    let delta = match action.as_str() {
        "helpful" => 0.1,
        "unhelpful" => -0.2,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported fact_feedback action: {other}"
            )));
        }
    };
    let mut result = None;
    for fact in &mut facts {
        if fact["id"].as_str() == Some(id.as_str()) {
            let current = fact["trust"].as_f64().unwrap_or(0.5);
            fact["trust"] = json!((current + delta).clamp(0.0, 1.0));
            fact["feedback"] = json!(action);
            fact["updatedAt"] = json!(now_iso());
            result = Some(fact.clone());
            break;
        }
    }
    let Some(fact) = result else {
        return Err(AppError::BadRequest(format!("fact not found: {id}")));
    };
    save_holographic_facts(store, &facts)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "fact": fact,
        "hermesHolographicDesktop": holographic_boundary(store)
    }))?)
}

pub(super) fn mirror_memory_write_to_holographic(
    store: &AppStore,
    action: &str,
    target: &str,
    content: &str,
) -> AppResult<Option<Value>> {
    if action != "add" || target == "holographic" || content.trim().is_empty() {
        return Ok(None);
    }
    let mut facts = load_holographic_facts(store)?;
    let category = if target == "user" || memory_content_looks_like_preference(content) {
        "user_pref"
    } else {
        "general"
    };
    let now = now_iso();
    let fact = json!({
        "id": new_id("fact"),
        "content": content.trim(),
        "category": category,
        "tags": ["memory_write"],
        "entities": extract_fact_entities(content),
        "trust": 0.5,
        "createdAt": now,
        "updatedAt": now,
        "source": "synthchat_memory_write_mirror",
        "sourceTarget": target
    });
    facts.push(fact.clone());
    save_holographic_facts(store, &facts)?;
    Ok(Some(fact))
}

pub(super) fn holographic_memory_prefetch_facts(
    store: &AppStore,
    query: &str,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let facts = load_holographic_facts(store)?;
    let payload = json!({"min_trust": 0.3});
    Ok(rank_holographic_facts(&facts, query, &payload)
        .into_iter()
        .take(limit.max(1))
        .collect())
}

pub(super) fn external_memory_provider_tool(tool_name: &str, payload: &Value) -> AppResult<String> {
    if matches!(
        tool_name,
        "byterover_status" | "brv_query" | "brv_curate" | "brv_status"
    ) {
        return byterover_tool(tool_name, payload);
    }
    let provider = provider_for_memory_tool(tool_name).unwrap_or("unknown");
    let required = required_env_for_provider(provider);
    let (configured, provider_runtime) = match provider {
        "supermemory" => {
            let config = supermemory_config_snapshot();
            if supermemory_live_requested(payload) {
                return supermemory_live_tool(tool_name, payload, &config);
            }
            (
                config["apiKeyConfigured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_supermemory_provider_desktop_v1",
                    "config": config,
                    "sdkPackage": "supermemory",
                    "clientClass": "supermemory.Supermemory",
                    "apiContract": supermemory_api_contract(),
                    "tools": ["supermemory_store", "supermemory_search", "supermemory_forget", "supermemory_profile"],
                    "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown"],
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes Supermemory env/file configuration and tool contract without importing the SDK or calling the Supermemory API. Add execute/live/apply:true plus confirmSupermemoryLive:true for confirmed REST execution."
                }),
            )
        }
        "mem0" => {
            let config = mem0_config_snapshot();
            if mem0_live_requested(payload) {
                return mem0_live_tool(tool_name, payload, &config);
            }
            (
                config["apiKeyConfigured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_mem0_provider_desktop_v1",
                    "config": config,
                    "sdkPackage": "mem0ai",
                    "clientClass": "mem0.MemoryClient",
                    "tools": ["mem0_profile", "mem0_search", "mem0_conclude"],
                    "hooks": ["queue_prefetch", "sync_turn", "shutdown"],
                    "apiContract": mem0_api_contract(),
                    "circuitBreaker": {
                        "failureThreshold": 5,
                        "cooldownSeconds": 120
                    },
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes Mem0 env/file configuration and tool contract without importing mem0ai or calling the Mem0 Platform API. Add execute/live/apply:true plus confirmMem0Live:true for confirmed REST execution."
                }),
            )
        }
        "honcho" => {
            let config = honcho_config_snapshot();
            if honcho_live_requested(payload) {
                return honcho_live_tool(tool_name, payload, &config);
            }
            (
                config["configured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_honcho_provider_desktop_v1",
                    "config": config,
                    "sdkPackage": "honcho-ai",
                    "clientClass": "honcho.Honcho",
                    "sessionManager": "plugins.memory.honcho.session.HonchoSessionManager",
                    "tools": ["honcho_profile", "honcho_search", "honcho_reasoning", "honcho_context", "honcho_conclude"],
                    "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown"],
                    "apiContract": honcho_api_contract(),
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes Honcho env/file configuration, host-block precedence, and tool contract without importing honcho-ai or calling the Honcho API. Add execute/live/apply:true plus confirmHonchoLive:true for confirmed REST execution."
                }),
            )
        }
        "openviking" => {
            let config = openviking_config_snapshot();
            if openviking_live_requested(payload) {
                return openviking_live_tool(tool_name, payload, &config);
            }
            (
                config["configured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_openviking_provider_desktop_v1",
                    "config": config,
                    "sdkPackage": null,
                    "httpClient": "httpx",
                    "restApi": openviking_rest_contract(),
                    "tools": ["viking_search", "viking_read", "viking_browse", "viking_remember", "viking_add_resource"],
                    "hooks": ["queue_prefetch", "prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown", "atexit_commit"],
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes OpenViking env configuration, tenant headers, REST endpoint contract, URI layout, and tool contract without importing httpx or calling the OpenViking server. Add execute/live/apply:true plus confirmOpenVikingLive:true for confirmed REST execution."
                }),
            )
        }
        "hindsight" => {
            let config = hindsight_config_snapshot();
            if hindsight_live_requested(payload) {
                return hindsight_live_tool(tool_name, payload, &config);
            }
            (
                config["configured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_hindsight_provider_desktop_v1",
                    "config": config,
                    "sdkPackage": "hindsight-client>=0.4.22",
                    "clientClasses": ["hindsight.Hindsight", "hindsight_embed.HindsightEmbedded"],
                    "tools": ["hindsight_retain", "hindsight_recall", "hindsight_reflect"],
                    "synthChatAliases": ["hindsight_remember", "hindsight_search", "hindsight_reflect"],
                    "hooks": ["prefetch", "sync_turn", "on_session_end", "on_session_switch", "shutdown", "atexit_shutdown"],
                    "apiContract": hindsight_api_contract(),
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes Hindsight profile/legacy/env configuration, bank/retain/recall/session lifecycle contract, and embedded-daemon readiness without importing hindsight-client or calling the Hindsight API. Add execute/live/apply:true plus confirmHindsightLive:true for confirmed REST execution."
                }),
            )
        }
        "retaindb" => {
            let config = retaindb_config_snapshot();
            if retaindb_live_requested(payload) {
                return retaindb_live_tool(tool_name, payload, &config);
            }
            (
                config["configured"].as_bool().unwrap_or(false),
                json!({
                    "kind": "hermes_retaindb_provider_desktop_v1",
                    "config": config,
                    "httpClient": "requests",
                    "tools": ["retaindb_profile", "retaindb_search", "retaindb_context", "retaindb_remember", "retaindb_forget", "retaindb_upload_file", "retaindb_list_files", "retaindb_read_file", "retaindb_ingest_file", "retaindb_delete_file"],
                    "synthChatAliases": ["retaindb_profile", "retaindb_search", "retaindb_store"],
                    "hooks": ["queue_prefetch", "prefetch", "sync_turn", "on_memory_write", "shutdown"],
                    "apiContract": retaindb_api_contract(),
                    "networkExecuted": false,
                    "boundary": "SynthChat resolves Hermes RetainDB env/config readiness, API route contract, write-behind queue, file-store tools, and prefetch lifecycle without calling the RetainDB API."
                }),
            )
        }
        _ => (
            required.iter().any(|name| std::env::var(name).is_ok()),
            json!({
                "kind": "hermes_memory_provider_external_boundary_v1",
                "nativeLocalProvider": "holographic/fact_store",
                "externalExecution": "requires provider SDK/API or Hermes Python plugin runtime"
            }),
        ),
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": tool_name,
        "provider": provider,
        "configured": configured,
        "payload": payload,
        "message": format!("{tool_name} is a Hermes memory-provider tool. SynthChat exposes the Hermes-compatible tool boundary; configure the external {provider} runtime/provider to execute it end-to-end."),
        "requiredEnv": required,
        "hermesMemoryProviderDesktop": provider_runtime
    }))?)
}

fn supermemory_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn supermemory_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(
        payload,
        &["confirmSupermemoryLive", "confirmLiveSupermemory"],
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "supermemory",
            "status": "live_confirmation_required",
            "configured": config["apiKeyConfigured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmSupermemoryLive:true",
            "message": "Supermemory live execution requires execute/live/apply plus confirmSupermemoryLive:true.",
            "planned": supermemory_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_supermemory_provider_desktop_v1",
                "config": config,
                "apiContract": supermemory_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["apiKeyConfigured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "supermemory",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["SUPERMEMORY_API_KEY"],
            "planned": supermemory_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_supermemory_provider_desktop_v1",
                "config": config,
                "apiContract": supermemory_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "supermemory_store" => supermemory_live_store(config, payload)?,
        "supermemory_search" => supermemory_live_search(config, payload)?,
        "supermemory_profile" => supermemory_live_profile(config, payload)?,
        "supermemory_forget" => supermemory_live_forget(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "supermemory",
                "status": "unsupported_live_alias",
                "message": "SynthChat live Supermemory execution currently supports the Hermes Supermemory tool set.",
                "planned": supermemory_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_supermemory_provider_desktop_v1",
                    "config": config,
                    "apiContract": supermemory_api_contract(),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "supermemory",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "fallbackUsed": false,
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_supermemory_provider_desktop_v1",
            "config": config,
            "apiContract": supermemory_api_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_supermemory_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "supportedAliases": ["supermemory_store", "supermemory_search", "supermemory_forget", "supermemory_profile"]
            }
        }
    }))?)
}

fn supermemory_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let container_tag = supermemory_container_tag(payload, config);
    let path = match tool_name {
        "supermemory_store" => "/v4/documents",
        "supermemory_search" => "/v4/search/memories",
        "supermemory_profile" => "/v4/profile",
        "supermemory_forget" => {
            if string_arg(payload, &["id", "memory_id", "memoryId"]).is_some() {
                "/v4/memories/{id}/forget"
            } else {
                "/v4/search/memories -> /v4/memories/{id}/forget"
            }
        }
        _ => "",
    };
    json!({
        "schema": "hermes_supermemory_live_plan_desktop_v1",
        "method": "POST",
        "path": path,
        "baseUrl": supermemory_base_url(config),
        "containerTag": container_tag,
        "sdkMapping": supermemory_sdk_mapping(tool_name),
        "networkExecuted": false
    })
}

fn supermemory_live_store(config: &Value, payload: &Value) -> AppResult<Value> {
    let content = string_arg(payload, &["content", "memory"]).ok_or_else(|| {
        AppError::BadRequest("supermemory_store live execution requires content".into())
    })?;
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "supermemory_store live content cannot be empty".into(),
        ));
    }
    let container_tag = supermemory_container_tag(payload, config);
    let entity_context = string_arg(payload, &["entity_context", "entityContext"])
        .map(|value| clamp_supermemory_entity_context(&value))
        .unwrap_or_else(|| supermemory_config_string(config, "entityContext", ""));
    let mut body = json!({
        "content": content.trim(),
        "container_tags": [container_tag]
    });
    if let Some(metadata) = payload.get("metadata").filter(|value| value.is_object()) {
        body["metadata"] = metadata.clone();
    }
    if !entity_context.trim().is_empty() {
        body["entity_context"] = json!(entity_context);
    }
    if let Some(custom_id) = string_arg(payload, &["custom_id", "customId"]) {
        if !custom_id.trim().is_empty() {
            body["custom_id"] = json!(custom_id.trim());
        }
    }
    let result = supermemory_request(config, "POST", "/v4/documents", Some(&body))?;
    let memory_id = result
        .get("id")
        .or_else(|| result.get("document_id"))
        .or_else(|| result.get("documentId"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "request": {"method": "POST", "path": "/v4/documents", "body": body, "sdkMethod": "client.documents.add"},
        "result": {"saved": true, "id": memory_id, "raw": result}
    }))
}

fn supermemory_live_search(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("supermemory_search live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "supermemory_search live query cannot be empty".into(),
        ));
    }
    let limit = payload
        .get("limit")
        .or_else(|| payload.get("top_k"))
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 20);
    let search_mode = string_arg(payload, &["search_mode", "searchMode"])
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "hybrid" | "memories" | "documents"))
        .unwrap_or_else(|| supermemory_config_string(config, "searchMode", "hybrid"));
    let container_tag = supermemory_container_tag(payload, config);
    let body = json!({
        "q": query.trim(),
        "container_tag": container_tag,
        "limit": limit,
        "search_mode": search_mode
    });
    let result = supermemory_request(config, "POST", "/v4/search/memories", Some(&body))?;
    let raw_results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let formatted = raw_results
        .into_iter()
        .map(|item| {
            let mut entry = json!({
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "content": item.get("memory")
                    .or_else(|| item.get("content"))
                    .cloned()
                    .unwrap_or(Value::Null)
            });
            if let Some(similarity) = item.get("similarity").and_then(Value::as_f64) {
                entry["similarity"] = json!((similarity * 100.0).round() as i64);
            }
            if let Some(metadata) = item.get("metadata") {
                entry["metadata"] = metadata.clone();
            }
            entry
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "request": {"method": "POST", "path": "/v4/search/memories", "body": body, "sdkMethod": "client.search.memories"},
        "result": {"results": formatted, "count": formatted.len(), "container_tag": body["container_tag"].clone(), "raw": result}
    }))
}

fn supermemory_live_profile(config: &Value, payload: &Value) -> AppResult<Value> {
    let container_tag = supermemory_container_tag(payload, config);
    let mut body = json!({"container_tag": container_tag});
    if let Some(query) = string_arg(payload, &["query", "q"]) {
        if !query.trim().is_empty() {
            body["q"] = json!(query.trim());
        }
    }
    let result = supermemory_request(config, "POST", "/v4/profile", Some(&body))?;
    let profile = result.get("profile").unwrap_or(&result);
    let static_items = profile
        .get("static")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dynamic_items = profile
        .get("dynamic")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut sections = Vec::new();
    if !static_items.is_empty() {
        sections.push(format!(
            "## User Profile (Persistent)\n{}",
            static_items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !dynamic_items.is_empty() {
        sections.push(format!(
            "## Recent Context\n{}",
            dynamic_items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    Ok(json!({
        "request": {"method": "POST", "path": "/v4/profile", "body": body, "sdkMethod": "client.profile"},
        "result": {
            "profile": sections.join("\n\n"),
            "static_count": static_items.len(),
            "dynamic_count": dynamic_items.len(),
            "container_tag": body["container_tag"].clone(),
            "raw": result
        }
    }))
}

fn supermemory_live_forget(config: &Value, payload: &Value) -> AppResult<Value> {
    if let Some(memory_id) = string_arg(payload, &["id", "memory_id", "memoryId"]) {
        if memory_id.trim().is_empty() {
            return Err(AppError::BadRequest(
                "supermemory_forget live id cannot be empty".into(),
            ));
        }
        let container_tag = supermemory_container_tag(payload, config);
        let body = json!({"container_tag": container_tag, "id": memory_id.trim()});
        let path = format!(
            "/v4/memories/{}/forget",
            url_encode_path_segment(memory_id.trim())
        );
        let result = supermemory_request(config, "POST", &path, Some(&body))?;
        return Ok(json!({
            "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "client.memories.forget"},
            "result": {"forgotten": true, "id": memory_id.trim(), "raw": result}
        }));
    }
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("supermemory_forget live execution requires id or query".into())
    })?;
    let search = supermemory_live_search(
        config,
        &json!({
            "query": query,
            "limit": 5,
            "container_tag": supermemory_container_tag(payload, config),
            "search_mode": payload.get("search_mode").or_else(|| payload.get("searchMode")).cloned().unwrap_or(Value::Null)
        }),
    )?;
    let first = search["result"]["results"]
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(Value::Null);
    let Some(memory_id) = first.get("id").and_then(Value::as_str).map(str::to_string) else {
        return Ok(json!({
            "request": search["request"].clone(),
            "result": {"success": false, "message": "No matching memory found to forget.", "search": search["result"].clone()}
        }));
    };
    let forget = supermemory_live_forget(
        config,
        &json!({
            "id": memory_id,
            "container_tag": supermemory_container_tag(payload, config)
        }),
    )?;
    let preview = first
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(100)
        .collect::<String>();
    Ok(json!({
        "request": {
            "method": "POST",
            "path": "/v4/search/memories -> /v4/memories/{id}/forget",
            "search": search["request"].clone(),
            "forget": forget["request"].clone(),
            "sdkMethod": "client.search.memories + client.memories.forget"
        },
        "result": {"success": true, "message": format!("Forgot: \"{preview}\""), "id": memory_id}
    }))
}

fn supermemory_request(
    config: &Value,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> AppResult<Value> {
    let base_url = supermemory_base_url(config);
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid SUPERMEMORY_BASE_URL: {error}")))?;
    url.set_path(path);
    let api_key = env::var("SUPERMEMORY_API_KEY")
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(
            "SUPERMEMORY_API_KEY is required for live Supermemory execution".into(),
        ));
    }
    let timeout = config["apiTimeout"]
        .as_f64()
        .unwrap_or(5.0)
        .clamp(0.5, 15.0);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis((timeout * 1000.0) as u64))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Supermemory client: {error}"))
        })?;
    let req_method = match method {
        "POST" => reqwest::Method::POST,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Supermemory method: {other}"
            )))
        }
    };
    let mut request = client
        .request(req_method, url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .header("x-sdk-runtime", "hermes-plugin")
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().map_err(|error| {
        AppError::BadRequest(format!("Supermemory {method} {path} failed: {error}"))
    })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Supermemory {method} {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn supermemory_base_url(config: &Value) -> String {
    env::var("SUPERMEMORY_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            config["baseUrl"]
                .as_str()
                .unwrap_or("https://api.supermemory.ai")
                .trim_end_matches('/')
                .to_string()
        })
}

fn supermemory_config_string(config: &Value, key: &str, default: &str) -> String {
    config[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn supermemory_container_tag(payload: &Value, config: &Value) -> String {
    string_arg(payload, &["container_tag", "containerTag"])
        .map(|value| sanitize_supermemory_tag(&value))
        .unwrap_or_else(|| supermemory_config_string(config, "resolvedContainerTag", "hermes"))
}

fn supermemory_api_contract() -> Value {
    json!({
        "sdkClient": "supermemory.Supermemory(api_key, timeout, max_retries=0)",
        "documentsAdd": "client.documents.add(content, container_tags, metadata?, entity_context?, custom_id?)",
        "searchMemories": "client.search.memories(q, container_tag, limit, search_mode)",
        "profile": "client.profile(container_tag, q?)",
        "forget": "client.memories.forget(container_tag, id)",
        "conversationIngest": "POST /v4/conversations",
        "restBridge": {
            "baseUrl": "SUPERMEMORY_BASE_URL or https://api.supermemory.ai",
            "documentsAdd": "POST /v4/documents",
            "searchMemories": "POST /v4/search/memories",
            "profile": "POST /v4/profile",
            "forget": "POST /v4/memories/{id}/forget",
            "authorization": "Authorization: Bearer SUPERMEMORY_API_KEY"
        }
    })
}

fn supermemory_sdk_mapping(tool_name: &str) -> &'static str {
    match tool_name {
        "supermemory_store" => "client.documents.add",
        "supermemory_search" => "client.search.memories",
        "supermemory_profile" => "client.profile",
        "supermemory_forget" => "client.memories.forget or search+forget_by_query",
        _ => "unknown",
    }
}

fn mem0_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn mem0_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(payload, &["confirmMem0Live", "confirmLiveMem0"]) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "mem0",
            "status": "live_confirmation_required",
            "configured": config["apiKeyConfigured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmMem0Live:true",
            "message": "Mem0 live execution requires execute/live/apply plus confirmMem0Live:true.",
            "planned": mem0_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_mem0_provider_desktop_v1",
                "config": config,
                "apiContract": mem0_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["apiKeyConfigured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "mem0",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["MEM0_API_KEY"],
            "planned": mem0_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_mem0_provider_desktop_v1",
                "config": config,
                "apiContract": mem0_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "mem0_profile" => mem0_live_profile(config)?,
        "mem0_search" => mem0_live_search(config, payload)?,
        "mem0_conclude" => mem0_live_conclude(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "mem0",
                "status": "unsupported_live_alias",
                "message": "SynthChat live Mem0 execution currently supports the Hermes Mem0 tool set.",
                "planned": mem0_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_mem0_provider_desktop_v1",
                    "config": config,
                    "apiContract": mem0_api_contract(),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "mem0",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "fallbackUsed": false,
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_mem0_provider_desktop_v1",
            "config": config,
            "apiContract": mem0_api_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_mem0_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "supportedAliases": ["mem0_profile", "mem0_search", "mem0_conclude"]
            }
        }
    }))?)
}

fn mem0_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let path = match tool_name {
        "mem0_profile" => "/v3/memories/",
        "mem0_search" => "/v3/memories/search/",
        "mem0_conclude" => "/v3/memories/add/",
        _ => "",
    };
    json!({
        "schema": "hermes_mem0_live_plan_desktop_v1",
        "method": "POST",
        "path": path,
        "baseUrl": mem0_base_url(config),
        "userId": mem0_config_string(config, "userId", "hermes-user"),
        "agentId": mem0_config_string(config, "agentId", "hermes"),
        "sdkMapping": mem0_sdk_mapping(tool_name),
        "networkExecuted": false,
        "query": string_arg(payload, &["query", "q"]).unwrap_or_default()
    })
}

fn mem0_live_profile(config: &Value) -> AppResult<Value> {
    let filters = json!({"user_id": mem0_config_string(config, "userId", "hermes-user")});
    let body = json!({"filters": filters});
    let result = mem0_request(config, "/v3/memories/", Some(&body))?;
    let memories = mem0_unwrap_results(&result);
    let lines = memories
        .iter()
        .filter_map(|memory| memory.get("memory").and_then(Value::as_str))
        .filter(|memory| !memory.trim().is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let formatted = if lines.is_empty() {
        "No memories stored yet.".to_string()
    } else {
        lines.join("\n")
    };
    Ok(json!({
        "request": {"method": "POST", "path": "/v3/memories/", "body": body, "sdkMethod": "client.get_all"},
        "result": {"result": formatted, "count": lines.len(), "raw": result}
    }))
}

fn mem0_live_search(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"])
        .ok_or_else(|| AppError::BadRequest("mem0_search live execution requires query".into()))?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "mem0_search live query cannot be empty".into(),
        ));
    }
    let top_k = payload
        .get("top_k")
        .or_else(|| payload.get("limit"))
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50);
    let rerank = payload
        .get("rerank")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body = json!({
        "query": query.trim(),
        "filters": {"user_id": mem0_config_string(config, "userId", "hermes-user")},
        "rerank": rerank,
        "top_k": top_k
    });
    let result = mem0_request(config, "/v3/memories/search/", Some(&body))?;
    let memories = mem0_unwrap_results(&result);
    let items = memories
        .into_iter()
        .map(|memory| {
            json!({
                "memory": memory.get("memory").cloned().unwrap_or(Value::Null),
                "score": memory.get("score")
                    .or_else(|| memory.get("similarity"))
                    .cloned()
                    .unwrap_or(Value::Null)
            })
        })
        .collect::<Vec<_>>();
    let response = if items.is_empty() {
        json!({"result": "No relevant memories found.", "raw": result})
    } else {
        json!({"results": items, "count": items.len(), "raw": result})
    };
    Ok(json!({
        "request": {"method": "POST", "path": "/v3/memories/search/", "body": body, "sdkMethod": "client.search"},
        "result": response
    }))
}

fn mem0_live_conclude(config: &Value, payload: &Value) -> AppResult<Value> {
    let conclusion =
        string_arg(payload, &["conclusion", "content", "memory"]).ok_or_else(|| {
            AppError::BadRequest("mem0_conclude live execution requires conclusion".into())
        })?;
    if conclusion.trim().is_empty() {
        return Err(AppError::BadRequest(
            "mem0_conclude live conclusion cannot be empty".into(),
        ));
    }
    let body = json!({
        "messages": [{"role": "user", "content": conclusion.trim()}],
        "user_id": mem0_config_string(config, "userId", "hermes-user"),
        "agent_id": mem0_config_string(config, "agentId", "hermes"),
        "infer": false
    });
    let result = mem0_request(config, "/v3/memories/add/", Some(&body))?;
    Ok(json!({
        "request": {"method": "POST", "path": "/v3/memories/add/", "body": body, "sdkMethod": "client.add(infer=false)"},
        "result": {"result": "Fact stored.", "raw": result}
    }))
}

fn mem0_request(config: &Value, path: &str, body: Option<&Value>) -> AppResult<Value> {
    let base_url = mem0_base_url(config);
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid MEM0_BASE_URL: {error}")))?;
    url.set_path(path);
    let api_key = mem0_api_key();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(
            "MEM0_API_KEY or $HERMES_HOME/mem0.json api_key is required for live Mem0 execution"
                .into(),
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Mem0 client: {error}")))?;
    let mut request = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, format!("Token {api_key}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .header("x-sdk-runtime", "hermes-plugin")
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .map_err(|error| AppError::BadRequest(format!("Mem0 POST {path} failed: {error}")))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Mem0 POST {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn mem0_unwrap_results(response: &Value) -> Vec<Value> {
    if let Some(items) = response.get("results").and_then(Value::as_array) {
        return items.clone();
    }
    if let Some(items) = response.get("memories").and_then(Value::as_array) {
        return items.clone();
    }
    response.as_array().cloned().unwrap_or_default()
}

fn mem0_base_url(config: &Value) -> String {
    env::var("MEM0_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            config["baseUrl"]
                .as_str()
                .unwrap_or("https://api.mem0.ai")
                .trim_end_matches('/')
                .to_string()
        })
}

fn mem0_config_string(config: &Value, key: &str, default: &str) -> String {
    config[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn mem0_api_key() -> String {
    let file_key = read_json_object(&hermes_home_dir().join("mem0.json"))
        .as_ref()
        .and_then(|value| value.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    file_key
        .or_else(|| {
            env::var("MEM0_API_KEY")
                .ok()
                .map(|value| value.trim().trim_start_matches("Token ").to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default()
}

fn mem0_api_contract() -> Value {
    json!({
        "sdkClient": "mem0.MemoryClient(api_key)",
        "profile": "client.get_all(filters={'user_id': user_id})",
        "search": "client.search(query, filters={'user_id': user_id}, rerank, top_k)",
        "conclude": "client.add([{'role':'user','content': conclusion}], user_id, agent_id, infer=False)",
        "prefetch": "client.search(query, filters={'user_id': user_id}, rerank=config.rerank, top_k=5)",
        "syncTurn": "client.add(user/assistant messages, user_id, agent_id)",
        "restBridge": {
            "baseUrl": "MEM0_BASE_URL or https://api.mem0.ai",
            "profile": "POST /v3/memories/",
            "search": "POST /v3/memories/search/",
            "conclude": "POST /v3/memories/add/",
            "authorization": "Authorization: Token MEM0_API_KEY"
        }
    })
}

fn mem0_sdk_mapping(tool_name: &str) -> &'static str {
    match tool_name {
        "mem0_profile" => "client.get_all",
        "mem0_search" => "client.search",
        "mem0_conclude" => "client.add(infer=false)",
        _ => "unknown",
    }
}

fn honcho_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn honcho_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(payload, &["confirmHonchoLive", "confirmLiveHoncho"]) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "honcho",
            "status": "live_confirmation_required",
            "configured": config["configured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmHonchoLive:true",
            "message": "Honcho live execution requires execute/live/apply plus confirmHonchoLive:true.",
            "planned": honcho_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_honcho_provider_desktop_v1",
                "config": config,
                "apiContract": honcho_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["configured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "honcho",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["HONCHO_API_KEY", "HONCHO_BASE_URL"],
            "planned": honcho_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_honcho_provider_desktop_v1",
                "config": config,
                "apiContract": honcho_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "honcho_profile" => honcho_live_profile(config, payload)?,
        "honcho_search" => honcho_live_search(config, payload)?,
        "honcho_reasoning" => honcho_live_reasoning(config, payload)?,
        "honcho_context" => honcho_live_context(config, payload)?,
        "honcho_conclude" => honcho_live_conclude(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "honcho",
                "status": "unsupported_live_alias",
                "message": "SynthChat live Honcho execution currently supports the Hermes Honcho tool set.",
                "planned": honcho_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_honcho_provider_desktop_v1",
                    "config": config,
                    "apiContract": honcho_api_contract(),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "honcho",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "fallbackUsed": false,
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_honcho_provider_desktop_v1",
            "config": config,
            "apiContract": honcho_api_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_honcho_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "supportedAliases": ["honcho_profile", "honcho_search", "honcho_reasoning", "honcho_context", "honcho_conclude"],
                "boundary": "Direct REST bridge for Honcho v3 endpoints; Python HonchoSessionManager lifecycle, lazy session cache, migration, and background flush remain separate provider-runtime concerns."
            }
        }
    }))?)
}

fn honcho_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let peer = honcho_peer_id(config, payload);
    let session = honcho_session_id(config, payload);
    let path = match tool_name {
        "honcho_profile" => format!("/v3/peers/{}/card", url_encode_path_segment(&peer)),
        "honcho_search" => format!("/v3/peers/{}/context", url_encode_path_segment(&peer)),
        "honcho_reasoning" => format!("/v3/peers/{}/chat", url_encode_path_segment(&peer)),
        "honcho_context" => format!("/v3/sessions/{}/context", url_encode_path_segment(&session)),
        "honcho_conclude" => {
            if string_arg(payload, &["delete_id", "deleteId"]).is_some() {
                "/v3/conclusions/{id}".into()
            } else {
                format!("/v3/peers/{}/conclusions", url_encode_path_segment(&peer))
            }
        }
        _ => String::new(),
    };
    json!({
        "schema": "hermes_honcho_live_plan_desktop_v1",
        "method": if tool_name == "honcho_conclude" && string_arg(payload, &["delete_id", "deleteId"]).is_some() { "DELETE" } else { "POST" },
        "path": path,
        "baseUrl": honcho_base_url(config),
        "workspace": honcho_config_string(config, "workspace", "hermes"),
        "peer": peer,
        "session": session,
        "sdkMapping": honcho_sdk_mapping(tool_name),
        "networkExecuted": false
    })
}

fn honcho_live_profile(config: &Value, payload: &Value) -> AppResult<Value> {
    let peer = honcho_peer_id(config, payload);
    if let Some(card) = payload.get("card").and_then(Value::as_array) {
        let card = card
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let body = json!({"card": card});
        let path = format!("/v3/peers/{}/card", url_encode_path_segment(&peer));
        let raw = honcho_request(config, "POST", &path, Some(&body))?;
        let updated = honcho_card_from_value(&raw);
        return Ok(json!({
            "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "peer.set_card"},
            "result": {"result": format!("Peer card updated ({} facts).", updated.len()), "card": updated, "raw": raw}
        }));
    }
    let path = format!("/v3/peers/{}/card", url_encode_path_segment(&peer));
    let raw = honcho_request(config, "GET", &path, None)?;
    let card = honcho_card_from_value(&raw);
    if card.is_empty() {
        return Ok(json!({
            "request": {"method": "GET", "path": path, "sdkMethod": "peer.get_card"},
            "result": {
                "result": [],
                "hint": "Peer card is empty. Honcho may need more observed conversation or dialectic cycles before it has facts.",
                "raw": raw
            }
        }));
    }
    Ok(json!({
        "request": {"method": "GET", "path": path, "sdkMethod": "peer.get_card"},
        "result": {"result": card, "raw": raw}
    }))
}

fn honcho_live_search(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("honcho_search live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "honcho_search live query cannot be empty".into(),
        ));
    }
    let peer = honcho_peer_id(config, payload);
    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("maxTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(800)
        .clamp(1, 2000);
    let body = json!({
        "search_query": query.trim(),
        "target": honcho_target_peer_id(config, payload),
        "max_tokens": max_tokens
    });
    let path = format!("/v3/peers/{}/context", url_encode_path_segment(&peer));
    let raw = honcho_request(config, "POST", &path, Some(&body))?;
    let result = honcho_context_text(&raw);
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "peer.context(search_query=...)"},
        "result": {"result": if result.is_empty() { "No relevant context found.".into() } else { result }, "raw": raw}
    }))
}

fn honcho_live_reasoning(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("honcho_reasoning live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "honcho_reasoning live query cannot be empty".into(),
        ));
    }
    let peer = honcho_peer_id(config, payload);
    let reasoning_level = string_arg(payload, &["reasoning_level", "reasoningLevel"])
        .map(|value| normalize_honcho_reasoning_level(&value, "low", true))
        .unwrap_or_else(|| honcho_config_string(config, "dialecticReasoningLevel", "low"));
    let body = json!({
        "query": query.trim(),
        "reasoning_level": reasoning_level,
        "target": honcho_target_peer_id(config, payload)
    });
    let path = format!("/v3/peers/{}/chat", url_encode_path_segment(&peer));
    let raw = honcho_request(config, "POST", &path, Some(&body))?;
    let result = raw
        .get("result")
        .or_else(|| raw.get("answer"))
        .or_else(|| raw.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "peer.chat"},
        "result": {"result": if result.is_empty() { "No result from Honcho.".into() } else { result }, "raw": raw}
    }))
}

fn honcho_live_context(config: &Value, payload: &Value) -> AppResult<Value> {
    let session = honcho_session_id(config, payload);
    let body = json!({
        "summary": true,
        "peer_target": honcho_target_peer_id(config, payload),
        "peer_perspective": honcho_peer_id(config, payload)
    });
    let path = format!("/v3/sessions/{}/context", url_encode_path_segment(&session));
    let raw = honcho_request(config, "POST", &path, Some(&body))?;
    let mut parts = Vec::new();
    if let Some(summary) = raw
        .get("summary")
        .and_then(|value| value.get("content").or(Some(value)))
        .and_then(Value::as_str)
    {
        if !summary.is_empty() {
            parts.push(format!("## Summary\n{summary}"));
        }
    }
    if let Some(representation) = raw
        .get("peer_representation")
        .or_else(|| raw.get("representation"))
        .and_then(Value::as_str)
    {
        if !representation.is_empty() {
            parts.push(format!("## Representation\n{representation}"));
        }
    }
    let card = honcho_card_from_value(&raw);
    if !card.is_empty() {
        parts.push(format!("## Card\n{}", card.join("\n")));
    }
    if let Some(messages) = raw.get("messages").and_then(Value::as_array) {
        let recent = messages
            .iter()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(|message| {
                let role = message
                    .get("role")
                    .or_else(|| message.get("peer_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let content = message.get("content").and_then(Value::as_str).unwrap_or("");
                if content.is_empty() {
                    None
                } else {
                    Some(format!(
                        "  [{role}] {}",
                        content.chars().take(200).collect::<String>()
                    ))
                }
            })
            .collect::<Vec<_>>();
        if !recent.is_empty() {
            parts.push(format!("## Recent messages\n{}", recent.join("\n")));
        }
    }
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "session.context"},
        "result": {"result": if parts.is_empty() { "No context available.".into() } else { parts.join("\n\n") }, "raw": raw}
    }))
}

fn honcho_live_conclude(config: &Value, payload: &Value) -> AppResult<Value> {
    let peer = honcho_peer_id(config, payload);
    let delete_id = string_arg(payload, &["delete_id", "deleteId"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let conclusion = string_arg(payload, &["conclusion", "content"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if delete_id.is_some() == conclusion.is_some() {
        return Err(AppError::BadRequest(
            "honcho_conclude requires exactly one of conclusion or delete_id".into(),
        ));
    }
    if let Some(delete_id) = delete_id {
        let path = format!("/v3/conclusions/{}", url_encode_path_segment(&delete_id));
        let raw = honcho_request(config, "DELETE", &path, None)?;
        return Ok(json!({
            "request": {"method": "DELETE", "path": path, "sdkMethod": "peer.conclusions_of(...).delete"},
            "result": {"result": format!("Conclusion {delete_id} deleted."), "raw": raw}
        }));
    }
    let conclusion = conclusion.unwrap_or_default();
    let body = json!({
        "conclusions": [{
            "content": conclusion,
            "session_id": honcho_session_id(config, payload)
        }],
        "target": honcho_target_peer_id(config, payload)
    });
    let path = format!("/v3/peers/{}/conclusions", url_encode_path_segment(&peer));
    let raw = honcho_request(config, "POST", &path, Some(&body))?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body, "sdkMethod": "peer.conclusions_of(...).create"},
        "result": {"result": format!("Conclusion saved for {}: {}", string_arg(payload, &["peer"]).unwrap_or_else(|| "user".into()), body["conclusions"][0]["content"].as_str().unwrap_or("")), "raw": raw}
    }))
}

fn honcho_request(
    config: &Value,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> AppResult<Value> {
    let base_url = honcho_base_url(config);
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid HONCHO_BASE_URL: {error}")))?;
    url.set_path(path);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs_f64(
            config["timeout"].as_f64().unwrap_or(30.0).clamp(0.5, 120.0),
        ))
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Honcho client: {error}")))?;
    let req_method = match method {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "DELETE" => reqwest::Method::DELETE,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Honcho method: {other}"
            )))
        }
    };
    let mut request = client
        .request(req_method.clone(), url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            "X-Honcho-Workspace",
            honcho_config_string(config, "workspace", "hermes"),
        )
        .header("x-sdk-runtime", "hermes-plugin");
    if let Some(api_key) = honcho_api_key(config) {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
    }
    if req_method == reqwest::Method::POST {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
    }
    let response = request
        .send()
        .map_err(|error| AppError::BadRequest(format!("Honcho {method} {path} failed: {error}")))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Honcho {method} {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn honcho_base_url(config: &Value) -> String {
    honcho_config_string(config, "baseUrl", "https://api.honcho.dev")
        .trim_end_matches('/')
        .to_string()
}

fn honcho_config_string(config: &Value, key: &str, default: &str) -> String {
    config[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn honcho_api_key(config: &Value) -> Option<String> {
    if config["apiKeySource"].as_str() == Some("file") {
        if let Some(value) = honcho_config_secret_from_file() {
            return Some(value);
        }
    }
    if let Some(value) = env::var("HONCHO_API_KEY")
        .ok()
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }
    if config["apiKeyConfigured"].as_bool().unwrap_or(false) {
        honcho_config_secret_from_file()
    } else {
        None
    }
}

fn honcho_config_secret_from_file() -> Option<String> {
    let config_path = hermes_home_dir().join("honcho.json");
    let file_config = read_json_object(&config_path);
    let host = env::var("HERMES_HONCHO_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hermes".into());
    let host_config = honcho_host_block(&file_config, &host);
    honcho_string(&file_config, &host_config, "apiKey")
}

fn honcho_peer_id(config: &Value, payload: &Value) -> String {
    let requested = string_arg(payload, &["peer"])
        .unwrap_or_else(|| "user".into())
        .trim()
        .to_string();
    if requested.eq_ignore_ascii_case("ai") {
        return honcho_config_string(config, "aiPeer", "hermes");
    }
    if requested.eq_ignore_ascii_case("user") {
        if let Some(peer_name) = config["peerName"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
        {
            return peer_name.trim().to_string();
        }
        return "user".into();
    }
    requested
}

fn honcho_target_peer_id(config: &Value, payload: &Value) -> String {
    honcho_peer_id(config, payload)
}

fn honcho_session_id(config: &Value, payload: &Value) -> String {
    string_arg(payload, &["session_id", "sessionId", "session"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}-default",
                honcho_config_string(config, "activeHost", "hermes")
            )
        })
}

fn honcho_card_from_value(value: &Value) -> Vec<String> {
    let candidate = value
        .get("card")
        .or_else(|| value.get("peer_card"))
        .or_else(|| value.get("result"))
        .unwrap_or(value);
    if let Some(items) = candidate.as_array() {
        return items
            .iter()
            .filter_map(|item| {
                item.as_str().map(ToString::to_string).or_else(|| {
                    item.get("content")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
            })
            .collect();
    }
    candidate
        .as_str()
        .map(|value| vec![value.to_string()])
        .unwrap_or_default()
}

fn honcho_context_text(value: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(representation) = value
        .get("representation")
        .or_else(|| value.get("peer_representation"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        parts.push(representation.to_string());
    }
    let card = honcho_card_from_value(value);
    if !card.is_empty() {
        parts.push(
            card.iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if parts.is_empty() {
        value
            .get("result")
            .or_else(|| value.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        parts.join("\n\n")
    }
}

fn honcho_api_contract() -> Value {
    json!({
        "sdkClient": "honcho-ai via plugins.memory.honcho.client.get_honcho_client",
        "sessionManager": "plugins.memory.honcho.session.HonchoSessionManager",
        "profileRead": "manager.get_peer_card(session_key, peer) -> peer.get_card(target?)",
        "profileWrite": "manager.set_peer_card(session_key, card, peer) -> peer.set_card(card, target?)",
        "search": "manager.search_context(session_key, query, max_tokens, peer) -> peer.context(search_query=..., target?)",
        "reasoning": "manager.dialectic_query(session_key, query, reasoning_level, peer) -> peer.chat(...)",
        "context": "manager.get_session_context(session_key, peer) -> session.context(summary=True, peer_target, peer_perspective)",
        "conclude": "manager.create/delete_conclusion -> peer.conclusions_of(target).create/delete",
        "restBridge": {
            "baseUrl": "HONCHO_BASE_URL or configured honcho.json baseUrl",
            "profileRead": "GET /v3/peers/{peer}/card",
            "profileWrite": "POST /v3/peers/{peer}/card",
            "search": "POST /v3/peers/{peer}/context",
            "reasoning": "POST /v3/peers/{peer}/chat",
            "context": "POST /v3/sessions/{session}/context",
            "concludeCreate": "POST /v3/peers/{peer}/conclusions",
            "concludeDelete": "DELETE /v3/conclusions/{id}",
            "authorization": "Authorization: Bearer HONCHO_API_KEY when configured"
        }
    })
}

fn honcho_sdk_mapping(tool_name: &str) -> &'static str {
    match tool_name {
        "honcho_profile" => "manager.get_peer_card or manager.set_peer_card",
        "honcho_search" => "manager.search_context",
        "honcho_reasoning" => "manager.dialectic_query",
        "honcho_context" => "manager.get_session_context",
        "honcho_conclude" => "manager.create_conclusion or manager.delete_conclusion",
        _ => "unknown",
    }
}

fn openviking_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn openviking_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(payload, &["confirmOpenVikingLive", "confirmLiveOpenViking"]) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "openviking",
            "status": "live_confirmation_required",
            "configured": config["configured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmOpenVikingLive:true",
            "message": "OpenViking live execution requires execute/live/apply plus confirmOpenVikingLive:true.",
            "planned": openviking_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_openviking_provider_desktop_v1",
                "config": config,
                "restApi": openviking_rest_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["configured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "openviking",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["OPENVIKING_ENDPOINT"],
            "planned": openviking_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_openviking_provider_desktop_v1",
                "config": config,
                "restApi": openviking_rest_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "viking_search" => openviking_live_search(config, payload)?,
        "viking_read" => openviking_live_read(config, payload)?,
        "viking_browse" => openviking_live_browse(config, payload)?,
        "viking_remember" => openviking_live_remember(config, payload)?,
        "viking_add_resource" => openviking_live_add_resource(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "openviking",
                "status": "unsupported_live_alias",
                "message": "SynthChat live OpenViking execution currently supports the Hermes OpenViking tool set.",
                "planned": openviking_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_openviking_provider_desktop_v1",
                    "config": config,
                    "restApi": openviking_rest_contract(),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "openviking",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "fallbackUsed": action.get("fallbackUsed").cloned().unwrap_or(Value::Bool(false)),
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_openviking_provider_desktop_v1",
            "config": config,
            "restApi": openviking_rest_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_openviking_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "supportedAliases": ["viking_search", "viking_read", "viking_browse", "viking_remember", "viking_add_resource"]
            }
        }
    }))?)
}

fn openviking_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let path = match tool_name {
        "viking_search" => "/api/v1/search/find",
        "viking_read" => match string_arg(payload, &["level"]).as_deref() {
            Some("abstract") => "/api/v1/content/abstract",
            Some("full") => "/api/v1/content/read",
            _ => "/api/v1/content/overview",
        },
        "viking_browse" => match string_arg(payload, &["action"]).as_deref() {
            Some("tree") => "/api/v1/fs/tree",
            Some("stat") => "/api/v1/fs/stat",
            _ => "/api/v1/fs/ls",
        },
        "viking_remember" => "/api/v1/content/write",
        "viking_add_resource" => "/api/v1/resources",
        _ => "",
    };
    json!({
        "schema": "hermes_openviking_live_plan_desktop_v1",
        "method": if matches!(tool_name, "viking_read" | "viking_browse") { "GET" } else { "POST" },
        "path": path,
        "endpoint": openviking_config_string(config, "endpoint", "http://127.0.0.1:1933"),
        "tenantHeaders": config["tenantHeaders"].clone(),
        "networkExecuted": false
    })
}

fn openviking_live_search(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("viking_search live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "viking_search live query cannot be empty".into(),
        ));
    }
    let mut body = json!({"query": query.trim()});
    let mode = string_arg(payload, &["mode"]).unwrap_or_else(|| "auto".into());
    if mode != "auto" {
        body["mode"] = json!(mode);
    }
    if let Some(scope) = string_arg(payload, &["scope", "target_uri", "targetUri"]) {
        if !scope.trim().is_empty() {
            body["target_uri"] = json!(scope.trim());
        }
    }
    if let Some(limit) = payload
        .get("limit")
        .or_else(|| payload.get("top_k"))
        .and_then(Value::as_u64)
    {
        body["top_k"] = json!(limit.clamp(1, 50));
    }
    let raw = openviking_request(config, "POST", "/api/v1/search/find", None, Some(&body))?;
    let result = raw.get("result").unwrap_or(&raw);
    let mut scored_entries: Vec<(f64, Value)> = Vec::new();
    if let Some(object) = result.as_object() {
        for (ctx_type, singular) in [
            ("memories", "memory"),
            ("resources", "resource"),
            ("skills", "skill"),
        ] {
            if let Some(items) = object.get(ctx_type).and_then(Value::as_array) {
                for item in items {
                    let score = item.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                    let mut entry = json!({
                        "uri": item.get("uri").cloned().unwrap_or(Value::Null),
                        "type": singular,
                        "score": (score * 1000.0).round() / 1000.0,
                        "abstract": item.get("abstract").cloned().unwrap_or(Value::String(String::new()))
                    });
                    if let Some(relations) = item.get("relations").and_then(Value::as_array) {
                        entry["related"] = json!(relations
                            .iter()
                            .take(3)
                            .filter_map(|relation| relation.get("uri").cloned())
                            .collect::<Vec<_>>());
                    }
                    scored_entries.push((score, entry));
                }
            }
        }
    }
    scored_entries.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let formatted = scored_entries
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();
    let total = result
        .get("total")
        .cloned()
        .unwrap_or_else(|| json!(formatted.len()));
    Ok(json!({
        "request": {"method": "POST", "path": "/api/v1/search/find", "body": body},
        "result": {"results": formatted, "total": total, "raw": raw}
    }))
}

fn openviking_live_read(config: &Value, payload: &Value) -> AppResult<Value> {
    let uri = string_arg(payload, &["uri"])
        .ok_or_else(|| AppError::BadRequest("viking_read live execution requires uri".into()))?;
    if uri.trim().is_empty() {
        return Err(AppError::BadRequest(
            "viking_read live uri cannot be empty".into(),
        ));
    }
    let level = string_arg(payload, &["level"]).unwrap_or_else(|| "overview".into());
    let endpoint = match level.as_str() {
        "abstract" => "/api/v1/content/abstract",
        "full" => "/api/v1/content/read",
        _ => "/api/v1/content/overview",
    };
    let query = [("uri", uri.trim().to_string())];
    let raw = openviking_request(config, "GET", endpoint, Some(&query), None)?;
    let result = raw.get("result").unwrap_or(&raw);
    let mut content = if let Some(text) = result.as_str() {
        text.to_string()
    } else {
        result
            .get("content")
            .or_else(|| result.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let max_len = match level.as_str() {
        "abstract" => 1200,
        "overview" => 4000,
        _ => 8000,
    };
    if content.len() > max_len {
        content.truncate(max_len);
        content.push_str("\n\n[... truncated, use a more specific URI or full level]");
    }
    Ok(json!({
        "request": {"method": "GET", "path": endpoint, "query": query},
        "result": {"uri": uri, "resolved_uri": uri, "level": level, "content": content, "raw": raw}
    }))
}

fn openviking_live_browse(config: &Value, payload: &Value) -> AppResult<Value> {
    let action = string_arg(payload, &["action"]).unwrap_or_else(|| "list".into());
    let path = string_arg(payload, &["path", "uri"]).unwrap_or_else(|| "viking://".into());
    let endpoint = match action.as_str() {
        "tree" => "/api/v1/fs/tree",
        "stat" => "/api/v1/fs/stat",
        _ => "/api/v1/fs/ls",
    };
    let query = [("uri", path.clone())];
    let raw = openviking_request(config, "GET", endpoint, Some(&query), None)?;
    let result = raw.get("result").unwrap_or(&raw);
    if matches!(action.as_str(), "list" | "tree") {
        let raw_entries = if let Some(items) = result.as_array() {
            items.clone()
        } else {
            result
                .get("entries")
                .or_else(|| result.get("items"))
                .or_else(|| result.get("children"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let entries = raw_entries
            .into_iter()
            .take(50)
            .map(|entry| {
                let uri = entry.get("uri").and_then(Value::as_str).unwrap_or("");
                let name = entry
                    .get("rel_path")
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| uri.rsplit('/').next().unwrap_or("").to_string());
                let is_dir = entry
                    .get("isDir")
                    .or_else(|| entry.get("is_dir"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| entry.get("type").and_then(Value::as_str) == Some("dir"));
                json!({
                    "name": name,
                    "uri": uri,
                    "type": if is_dir { "dir" } else { "file" },
                    "abstract": entry.get("abstract").cloned().unwrap_or(Value::String(String::new()))
                })
            })
            .collect::<Vec<_>>();
        return Ok(json!({
            "request": {"method": "GET", "path": endpoint, "query": query},
            "result": {"path": path, "entries": entries, "raw": raw}
        }));
    }
    Ok(json!({
        "request": {"method": "GET", "path": endpoint, "query": query},
        "result": result.clone()
    }))
}

fn openviking_live_remember(config: &Value, payload: &Value) -> AppResult<Value> {
    let content = string_arg(payload, &["content", "memory"]).ok_or_else(|| {
        AppError::BadRequest("viking_remember live execution requires content".into())
    })?;
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "viking_remember live content cannot be empty".into(),
        ));
    }
    let category = string_arg(payload, &["category"]).unwrap_or_default();
    let subdir = match category.as_str() {
        "entity" => "entities",
        "event" => "events",
        "case" => "cases",
        "pattern" => "patterns",
        _ => "preferences",
    };
    let uri = string_arg(payload, &["uri"]).unwrap_or_else(|| {
        format!(
            "viking://user/{}/memories/{}/mem_{}.md",
            openviking_config_string(config, "user", "default"),
            subdir,
            new_id("viking")
                .trim_start_matches("viking_")
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    let body = json!({"uri": uri, "content": content.trim(), "mode": "create"});
    let raw = openviking_request(config, "POST", "/api/v1/content/write", None, Some(&body))?;
    let written = raw
        .get("result")
        .and_then(|result| result.get("written_bytes"))
        .cloned()
        .unwrap_or_else(|| json!(0));
    Ok(json!({
        "request": {"method": "POST", "path": "/api/v1/content/write", "body": body},
        "result": {
            "status": "stored",
            "message": format!("Memory stored ({written}b) and queued for vector indexing."),
            "uri": body["uri"].clone(),
            "raw": raw
        }
    }))
}

fn openviking_live_add_resource(config: &Value, payload: &Value) -> AppResult<Value> {
    let url = string_arg(payload, &["url", "path"]).ok_or_else(|| {
        AppError::BadRequest("viking_add_resource live execution requires url".into())
    })?;
    if url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "viking_add_resource live url cannot be empty".into(),
        ));
    }
    if string_arg(payload, &["to"]).is_some() && string_arg(payload, &["parent"]).is_some() {
        return Err(AppError::BadRequest(
            "viking_add_resource cannot specify both to and parent".into(),
        ));
    }
    let mut body = serde_json::Map::new();
    for key in ["reason", "to", "parent", "instruction"] {
        if let Some(value) = string_arg(payload, &[key]) {
            if !value.trim().is_empty() {
                body.insert(key.into(), json!(value.trim()));
            }
        }
    }
    for key in ["wait"] {
        if let Some(value) = payload.get(key).and_then(Value::as_bool) {
            body.insert(key.into(), json!(value));
        }
    }
    if let Some(timeout) = payload.get("timeout").and_then(Value::as_f64) {
        body.insert("timeout".into(), json!(timeout.clamp(0.5, 600.0)));
    }
    let source = url.trim();
    let mut upload_request = Value::Null;
    if let Some(local_path) = openviking_local_resource_path(source)? {
        if !local_path.exists() {
            return Err(AppError::BadRequest(format!(
                "viking_add_resource local path does not exist: {}",
                local_path.to_string_lossy()
            )));
        }
        let mut cleanup_zip: Option<PathBuf> = None;
        let upload_path = if local_path.is_dir() {
            let zip_path = openviking_zip_directory(&local_path)?;
            cleanup_zip = Some(zip_path.clone());
            zip_path
        } else if local_path.is_file() {
            local_path.clone()
        } else {
            return Err(AppError::BadRequest(format!(
                "unsupported OpenViking local resource path: {}",
                local_path.to_string_lossy()
            )));
        };
        let source_name = local_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        let filename = upload_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        let bytes = fs::read(&upload_path).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to read OpenViking local resource {}: {error}",
                upload_path.to_string_lossy()
            ))
        })?;
        let mime = retaindb_guess_mime(&filename);
        let upload = openviking_temp_upload(config, &bytes, &filename, &mime);
        if let Some(path) = cleanup_zip {
            let _ = fs::remove_file(path);
        }
        let upload = upload?;
        let temp_file_id = upload
            .get("result")
            .and_then(|result| result.get("temp_file_id"))
            .or_else(|| upload.get("temp_file_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("OpenViking temp upload did not return temp_file_id".into())
            })?
            .to_string();
        body.insert("source_name".into(), json!(source_name));
        body.insert("temp_file_id".into(), json!(temp_file_id));
        upload_request = json!({
            "method": "POST",
            "path": "/api/v1/resources/temp_upload",
            "filename": filename,
            "sourceName": source_name,
            "sourcePath": local_path.to_string_lossy().to_string(),
            "sourceWasDirectory": local_path.is_dir(),
            "mime": mime,
            "bytes": bytes.len(),
            "result": upload
        });
    } else {
        body.insert("path".into(), json!(source));
    }
    let body = Value::Object(body);
    let raw = openviking_request(config, "POST", "/api/v1/resources", None, Some(&body))?;
    let result = raw.get("result").unwrap_or(&raw);
    Ok(json!({
        "request": {"method": "POST", "path": "/api/v1/resources", "body": body, "tempUpload": upload_request},
        "result": {
            "status": "added",
            "root_uri": result.get("root_uri").cloned().unwrap_or(Value::String(String::new())),
            "message": "Resource queued for processing. Use viking_search after a moment to find it.",
            "raw": raw
        }
    }))
}

fn openviking_local_resource_path(source: &str) -> AppResult<Option<PathBuf>> {
    if source.starts_with("file://") {
        let url = reqwest::Url::parse(source)
            .map_err(|error| AppError::BadRequest(format!("invalid file URI: {error}")))?;
        return url
            .to_file_path()
            .map(Some)
            .map_err(|_| AppError::BadRequest(format!("invalid local file URI: {source}")));
    }
    if openviking_is_remote_resource(source) {
        return Ok(None);
    }
    let path = PathBuf::from(source);
    if path.is_absolute()
        || source.starts_with("./")
        || source.starts_with("../")
        || source.starts_with(".\\")
        || source.starts_with("..\\")
        || source.starts_with("~/")
        || source.starts_with("~\\")
        || path.exists()
    {
        return Ok(Some(expand_home_path(&path)));
    }
    Ok(None)
}

fn openviking_zip_directory(dir_path: &Path) -> AppResult<PathBuf> {
    let root = fs::canonicalize(dir_path).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to resolve OpenViking directory {}: {error}",
            dir_path.to_string_lossy()
        ))
    })?;
    let zip_path = env::temp_dir().join(format!("openviking_upload_{}.zip", new_id("zip")));
    let file = fs::File::create(&zip_path).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to create OpenViking directory zip {}: {error}",
            zip_path.to_string_lossy()
        ))
    })?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    openviking_zip_directory_entries(&root, &root, &mut zip, options)?;
    zip.finish().map_err(|error| {
        AppError::BadRequest(format!(
            "failed to finish OpenViking directory zip {}: {error}",
            zip_path.to_string_lossy()
        ))
    })?;
    Ok(zip_path)
}

fn openviking_zip_directory_entries<W: std::io::Write + std::io::Seek>(
    root: &Path,
    current: &Path,
    zip: &mut zip::ZipWriter<W>,
    options: zip::write::SimpleFileOptions,
) -> AppResult<()> {
    let entries = fs::read_dir(current).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read OpenViking directory {}: {error}",
            current.to_string_lossy()
        ))
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to inspect OpenViking directory entry {}: {error}",
                path.to_string_lossy()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            openviking_zip_directory_entries(root, &path, zip, options)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let resolved = fs::canonicalize(&path).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to resolve OpenViking file {}: {error}",
                path.to_string_lossy()
            ))
        })?;
        let Ok(relative) = resolved.strip_prefix(root) else {
            continue;
        };
        let archive_name = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if archive_name.is_empty() {
            continue;
        }
        zip.start_file(archive_name, options).map_err(|error| {
            AppError::BadRequest(format!("failed to add file to OpenViking zip: {error}"))
        })?;
        let mut file = fs::File::open(&resolved).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to open OpenViking file {}: {error}",
                resolved.to_string_lossy()
            ))
        })?;
        std::io::copy(&mut file, zip).map_err(|error| {
            AppError::BadRequest(format!("failed to write file to OpenViking zip: {error}"))
        })?;
    }
    Ok(())
}

fn openviking_is_remote_resource(source: &str) -> bool {
    let lowered = source.to_ascii_lowercase();
    lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("ssh://")
        || lowered.starts_with("git://")
        || source.starts_with("git@")
}

fn expand_home_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
            let rest = text
                .trim_start_matches('~')
                .trim_start_matches('/')
                .trim_start_matches('\\');
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

fn openviking_temp_upload(
    config: &Value,
    bytes: &[u8],
    filename: &str,
    mime: &str,
) -> AppResult<Value> {
    let base_url = openviking_config_string(config, "endpoint", "http://127.0.0.1:1933");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid OPENVIKING_ENDPOINT: {error}")))?;
    url.set_path("/api/v1/resources/temp_upload");
    let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename.to_string())
        .mime_str(mime)
        .map_err(|error| {
            AppError::BadRequest(format!("invalid OpenViking upload MIME: {error}"))
        })?;
    let form = reqwest::blocking::multipart::Form::new().part("file", part);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build OpenViking upload client: {error}"))
        })?;
    let mut request = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            "X-OpenViking-Agent",
            openviking_config_string(config, "agent", "hermes"),
        )
        .header(
            "X-OpenViking-Account",
            openviking_config_string(config, "account", "default"),
        )
        .header(
            "X-OpenViking-User",
            openviking_config_string(config, "user", "default"),
        );
    if let Ok(api_key) = env::var("OPENVIKING_API_KEY") {
        let api_key = api_key.trim().trim_start_matches("Bearer ");
        if !api_key.is_empty() {
            request = request
                .header("X-API-Key", api_key)
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
        }
    }
    let response = request.multipart(form).send().map_err(|error| {
        AppError::BadRequest(format!(
            "OpenViking POST /api/v1/resources/temp_upload failed: {error}"
        ))
    })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "OpenViking POST /api/v1/resources/temp_upload failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    if parsed.get("status").and_then(Value::as_str) == Some("error") {
        return Err(AppError::BadRequest(format!(
            "OpenViking POST /api/v1/resources/temp_upload returned error: {parsed}"
        )));
    }
    Ok(parsed)
}

fn openviking_request(
    config: &Value,
    method: &str,
    path: &str,
    query: Option<&[(&str, String)]>,
    body: Option<&Value>,
) -> AppResult<Value> {
    let base_url = openviking_config_string(config, "endpoint", "http://127.0.0.1:1933");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid OPENVIKING_ENDPOINT: {error}")))?;
    url.set_path(path);
    if let Some(query) = query {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build OpenViking client: {error}"))
        })?;
    let req_method = match method {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported OpenViking method: {other}"
            )))
        }
    };
    let mut request = client
        .request(req_method.clone(), url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            "X-OpenViking-Agent",
            openviking_config_string(config, "agent", "hermes"),
        )
        .header(
            "X-OpenViking-Account",
            openviking_config_string(config, "account", "default"),
        )
        .header(
            "X-OpenViking-User",
            openviking_config_string(config, "user", "default"),
        );
    if let Ok(api_key) = env::var("OPENVIKING_API_KEY") {
        let api_key = api_key.trim().trim_start_matches("Bearer ");
        if !api_key.is_empty() {
            request = request
                .header("X-API-Key", api_key)
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
        }
    }
    if req_method == reqwest::Method::POST {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
    }
    let response = request.send().map_err(|error| {
        AppError::BadRequest(format!("OpenViking {method} {path} failed: {error}"))
    })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "OpenViking {method} {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    if parsed.get("status").and_then(Value::as_str) == Some("error") {
        return Err(AppError::BadRequest(format!(
            "OpenViking {method} {path} returned error: {parsed}"
        )));
    }
    Ok(parsed)
}

fn openviking_config_string(config: &Value, key: &str, default: &str) -> String {
    config[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn retaindb_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn payload_bool(payload: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .any(|key| payload.get(*key).and_then(Value::as_bool).unwrap_or(false))
}

fn hindsight_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn hindsight_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(payload, &["confirmHindsightLive", "confirmLiveHindsight"]) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "hindsight",
            "status": "live_confirmation_required",
            "configured": config["configured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmHindsightLive:true",
            "message": "Hindsight live execution requires execute/live/apply plus confirmHindsightLive:true.",
            "planned": hindsight_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_hindsight_provider_desktop_v1",
                "config": config,
                "apiContract": hindsight_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["configured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "hindsight",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["HINDSIGHT_API_KEY"],
            "planned": hindsight_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_hindsight_provider_desktop_v1",
                "config": config,
                "apiContract": hindsight_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "hindsight_remember" | "hindsight_retain" => hindsight_live_retain(config, payload)?,
        "hindsight_search" | "hindsight_recall" => hindsight_live_recall(config, payload)?,
        "hindsight_reflect" => hindsight_live_reflect(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "hindsight",
                "status": "unsupported_live_alias",
                "message": "SynthChat live Hindsight execution supports hindsight_remember/retain, hindsight_search/recall, and hindsight_reflect.",
                "planned": hindsight_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_hindsight_provider_desktop_v1",
                    "config": config,
                    "apiContract": hindsight_api_contract(),
                    "networkExecuted": false
                }
            }))?);
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "hindsight",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_hindsight_provider_desktop_v1",
            "config": config,
            "apiContract": hindsight_api_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_hindsight_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "apiSurface": "Hindsight HTTP API /v1/default/banks/{bank_id}",
                "sdkMapping": {
                    "hindsight_remember": "client.aretain(...) via POST /memories",
                    "hindsight_search": "client.arecall(...) via POST /memories/recall",
                    "hindsight_reflect": "client.areflect(...) via POST /reflect"
                },
                "supportedAliases": ["hindsight_remember", "hindsight_retain", "hindsight_search", "hindsight_recall", "hindsight_reflect"]
            }
        }
    }))?)
}

fn hindsight_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let bank_id = hindsight_config_string(config, "bankId", "hermes");
    let bank_path = format!("/v1/default/banks/{}", url_encode_path_segment(&bank_id));
    let path = match tool_name {
        "hindsight_remember" | "hindsight_retain" => format!("{bank_path}/memories"),
        "hindsight_search" | "hindsight_recall" => format!("{bank_path}/memories/recall"),
        "hindsight_reflect" => format!("{bank_path}/reflect"),
        _ => String::new(),
    };
    json!({
        "schema": "hermes_hindsight_live_plan_desktop_v1",
        "method": "POST",
        "path": path,
        "baseUrl": config["apiUrl"].clone(),
        "bankId": bank_id,
        "budget": config["budget"].clone(),
        "recallMaxTokens": config["recallMaxTokens"].clone(),
        "payloadKeys": payload.as_object().map(|map| map.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "appendCapabilityProbe": "/version",
        "networkExecuted": false
    })
}

fn hindsight_live_retain(config: &Value, payload: &Value) -> AppResult<Value> {
    let content =
        string_arg(payload, &["content", "summary", "memory", "fact"]).ok_or_else(|| {
            AppError::BadRequest("hindsight_remember live execution requires content".into())
        })?;
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "hindsight_remember live content cannot be empty".into(),
        ));
    }
    let mut item = serde_json::Map::new();
    item.insert("content".into(), json!(content));
    if let Some(context) = string_arg(payload, &["context", "label"]) {
        if !context.trim().is_empty() {
            item.insert("context".into(), json!(context));
        }
    }
    if let Some(document_id) = string_arg(payload, &["document_id", "documentId"]) {
        if !document_id.trim().is_empty() {
            item.insert("document_id".into(), json!(document_id));
        }
    }
    let mut tags = config["retainTags"].as_array().cloned().unwrap_or_default();
    for tag in hindsight_tags(payload.get("tags").cloned()) {
        if !tags.iter().any(|existing| existing.as_str() == Some(&tag)) {
            tags.push(json!(tag));
        }
    }
    if !tags.is_empty() {
        item.insert("tags".into(), Value::Array(tags));
    }
    if let Some(metadata) = payload.get("metadata").filter(|value| value.is_object()) {
        item.insert("metadata".into(), metadata.clone());
    } else {
        let mut metadata = serde_json::Map::new();
        metadata.insert("retained_at".into(), json!(now_iso()));
        if let Some(source) = config["retainSource"]
            .as_str()
            .filter(|value| !value.is_empty())
        {
            metadata.insert("source".into(), json!(source));
        }
        item.insert("metadata".into(), Value::Object(metadata));
    }
    let retain_async = payload
        .get("retain_async")
        .or_else(|| payload.get("async"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| config["retainAsync"].as_bool().unwrap_or(true));
    let body = json!({
        "async": retain_async,
        "items": [Value::Object(item)]
    });
    let path = hindsight_live_plan("hindsight_remember", payload, config)["path"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let result = hindsight_request(config, "POST", &path, Some(&body), Duration::from_secs(30))?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body},
        "result": result
    }))
}

fn hindsight_live_recall(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("hindsight_search live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "hindsight_search live query cannot be empty".into(),
        ));
    }
    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("maxTokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| config["recallMaxTokens"].as_u64().unwrap_or(4096))
        .clamp(128, 32000);
    let tags = if payload.get("tags").is_some() {
        hindsight_tags(payload.get("tags").cloned())
    } else {
        config["recallTags"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let types = if payload.get("types").is_some() {
        hindsight_tags(payload.get("types").cloned())
    } else {
        config["recallTypes"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mut body = serde_json::Map::new();
    body.insert("query".into(), json!(query));
    body.insert(
        "budget".into(),
        json!(string_arg(payload, &["budget"])
            .map(|value| normalize_hindsight_budget(&value))
            .unwrap_or_else(|| hindsight_config_string(config, "budget", "mid"))),
    );
    body.insert("max_tokens".into(), json!(max_tokens));
    if !tags.is_empty() {
        body.insert("tags".into(), json!(tags));
        body.insert(
            "tags_match".into(),
            json!(string_arg(payload, &["tags_match", "tagsMatch"])
                .unwrap_or_else(|| hindsight_config_string(config, "recallTagsMatch", "any"))),
        );
    }
    if !types.is_empty() {
        body.insert("types".into(), json!(types));
    }
    if let Some(trace) = payload.get("trace").and_then(Value::as_bool) {
        body.insert("trace".into(), json!(trace));
    }
    let body = Value::Object(body);
    let path = hindsight_live_plan("hindsight_search", payload, config)["path"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let result = hindsight_request(config, "POST", &path, Some(&body), Duration::from_secs(30))?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body},
        "result": result
    }))
}

fn hindsight_live_reflect(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "summary", "q"]).ok_or_else(|| {
        AppError::BadRequest("hindsight_reflect live execution requires query".into())
    })?;
    if query.trim().is_empty() {
        return Err(AppError::BadRequest(
            "hindsight_reflect live query cannot be empty".into(),
        ));
    }
    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("maxTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(4096)
        .clamp(128, 32000);
    let mut body = serde_json::Map::new();
    body.insert("query".into(), json!(query));
    body.insert(
        "budget".into(),
        json!(string_arg(payload, &["budget"])
            .map(|value| normalize_hindsight_budget(&value))
            .unwrap_or_else(|| hindsight_config_string(config, "budget", "mid"))),
    );
    body.insert("max_tokens".into(), json!(max_tokens));
    let tags = hindsight_tags(payload.get("tags").cloned());
    if !tags.is_empty() {
        body.insert("tags".into(), json!(tags));
        body.insert(
            "tags_match".into(),
            json!(string_arg(payload, &["tags_match", "tagsMatch"])
                .unwrap_or_else(|| "any".into())),
        );
    }
    if let Some(include) = payload.get("include").filter(|value| value.is_object()) {
        body.insert("include".into(), include.clone());
    }
    let body = Value::Object(body);
    let path = hindsight_live_plan("hindsight_reflect", payload, config)["path"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let result = hindsight_request(config, "POST", &path, Some(&body), Duration::from_secs(60))?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body},
        "result": result
    }))
}

fn hindsight_request(
    config: &Value,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> AppResult<Value> {
    let base_url = hindsight_config_string(config, "apiUrl", "https://api.hindsight.vectorize.io");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid HINDSIGHT_API_URL: {error}")))?;
    url.set_path(path);
    let api_key = hindsight_api_key().unwrap_or_default();
    if api_key.is_empty() && config["mode"].as_str() == Some("cloud") {
        return Err(AppError::BadRequest(
            "HINDSIGHT_API_KEY is required for live Hindsight cloud execution".into(),
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Hindsight client: {error}"))
        })?;
    let req_method = match method {
        "POST" => reqwest::Method::POST,
        "GET" => reqwest::Method::GET,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Hindsight method: {other}"
            )))
        }
    };
    let mut request = client
        .request(req_method.clone(), url)
        .header(reqwest::header::ACCEPT, "application/json");
    if !api_key.is_empty() {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
    }
    if req_method == reqwest::Method::POST {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
    }
    let response = request.send().map_err(|error| {
        AppError::BadRequest(format!("Hindsight {method} {path} failed: {error}"))
    })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Hindsight {method} {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn hindsight_api_key() -> Option<String> {
    let profile_path = hermes_home_dir().join("hindsight").join("config.json");
    let profile_config = read_json_object(&profile_path);
    let legacy_config = if profile_config.is_some() {
        None
    } else {
        read_json_object(&hindsight_legacy_config_path())
    };
    let config = profile_config.or(legacy_config);
    hindsight_string(&config, "apiKey")
        .or_else(|| hindsight_string(&config, "api_key"))
        .or_else(|| env::var("HINDSIGHT_API_KEY").ok())
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .filter(|value| !value.is_empty())
}

fn hindsight_config_string(config: &Value, key: &str, default: &str) -> String {
    config[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn retaindb_live_tool(tool_name: &str, payload: &Value, config: &Value) -> AppResult<String> {
    if !payload_bool(payload, &["confirmRetainDbLive", "confirmLiveRetainDb"]) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "retaindb",
            "status": "live_confirmation_required",
            "configured": config["configured"].as_bool().unwrap_or(false),
            "requiredFlag": "confirmRetainDbLive:true",
            "message": "RetainDB live execution requires execute/live/apply plus confirmRetainDbLive:true.",
            "planned": retaindb_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_retaindb_provider_desktop_v1",
                "config": config,
                "apiContract": retaindb_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    if !config["configured"].as_bool().unwrap_or(false) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "retaindb",
            "status": "not_configured",
            "configured": false,
            "requiredEnv": ["RETAINDB_API_KEY"],
            "planned": retaindb_live_plan(tool_name, payload, config),
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_retaindb_provider_desktop_v1",
                "config": config,
                "apiContract": retaindb_api_contract(),
                "networkExecuted": false
            }
        }))?);
    }
    let action = match tool_name {
        "retaindb_profile" => retaindb_live_profile(config, payload)?,
        "retaindb_search" => retaindb_live_search(config, payload)?,
        "retaindb_context" => retaindb_live_context(config, payload)?,
        "retaindb_store" | "retaindb_remember" => retaindb_live_store(config, payload)?,
        "retaindb_forget" => retaindb_live_forget(config, payload)?,
        "retaindb_upload_file" => retaindb_live_upload_file(config, payload)?,
        "retaindb_list_files" => retaindb_live_list_files(config, payload)?,
        "retaindb_read_file" => retaindb_live_read_file(config, payload)?,
        "retaindb_ingest_file" => retaindb_live_ingest_file(config, payload)?,
        "retaindb_delete_file" => retaindb_live_delete_file(config, payload)?,
        "retaindb_ingest_session" => retaindb_live_ingest_session(config, payload)?,
        "retaindb_agent_model" => retaindb_live_agent_model(config, payload)?,
        "retaindb_seed_agent" => retaindb_live_seed_agent(config, payload)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "retaindb",
                "status": "unsupported_live_alias",
                "message": "SynthChat live RetainDB execution currently supports RetainDB memory and file tools exposed by the Hermes provider.",
                "planned": retaindb_live_plan(tool_name, payload, config),
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_retaindb_provider_desktop_v1",
                    "config": config,
                    "apiContract": retaindb_api_contract(),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": tool_name,
        "provider": "retaindb",
        "status": "executed",
        "configured": true,
        "result": action["result"].clone(),
        "request": action["request"].clone(),
        "fallbackUsed": action["fallbackUsed"].clone(),
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_retaindb_provider_desktop_v1",
            "config": config,
            "apiContract": retaindb_api_contract(),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_retaindb_live_execution_desktop_v1",
                "confirmed": true,
                "httpClient": "reqwest::blocking",
                "supportedAliases": ["retaindb_profile", "retaindb_search", "retaindb_context", "retaindb_store", "retaindb_remember", "retaindb_forget", "retaindb_upload_file", "retaindb_list_files", "retaindb_read_file", "retaindb_ingest_file", "retaindb_delete_file", "retaindb_ingest_session", "retaindb_agent_model", "retaindb_seed_agent"]
            }
        }
    }))?)
}

fn retaindb_live_plan(tool_name: &str, payload: &Value, config: &Value) -> Value {
    let user_id = retaindb_user_id(payload);
    let session_id = retaindb_session_id(payload);
    let path = match tool_name {
        "retaindb_profile" => format!("/v1/memory/profile/{}", url_encode_path_segment(&user_id)),
        "retaindb_search" => "/v1/memory/search".into(),
        "retaindb_context" => "/v1/context/query".into(),
        "retaindb_store" | "retaindb_remember" => "/v1/memory".into(),
        "retaindb_forget" => string_arg(payload, &["memory_id", "memoryId", "id"])
            .map(|memory_id| format!("/v1/memory/{}", url_encode_path_segment(&memory_id)))
            .unwrap_or_else(|| "/v1/memory/{memory_id}".into()),
        "retaindb_upload_file" => "/v1/files".into(),
        "retaindb_list_files" => "/v1/files".into(),
        "retaindb_read_file" => string_arg(payload, &["file_id", "fileId", "id"])
            .map(|file_id| format!("/v1/files/{}/content", url_encode_path_segment(&file_id)))
            .unwrap_or_else(|| "/v1/files/{file_id}/content".into()),
        "retaindb_ingest_file" => string_arg(payload, &["file_id", "fileId", "id"])
            .map(|file_id| format!("/v1/files/{}/ingest", url_encode_path_segment(&file_id)))
            .unwrap_or_else(|| "/v1/files/{file_id}/ingest".into()),
        "retaindb_delete_file" => string_arg(payload, &["file_id", "fileId", "id"])
            .map(|file_id| format!("/v1/files/{}", url_encode_path_segment(&file_id)))
            .unwrap_or_else(|| "/v1/files/{file_id}".into()),
        "retaindb_ingest_session" => "/v1/memory/ingest/session".into(),
        "retaindb_agent_model" => {
            format!(
                "/v1/memory/agent/{}/model",
                url_encode_path_segment(&retaindb_agent_id(payload))
            )
        }
        "retaindb_seed_agent" => {
            format!(
                "/v1/memory/agent/{}/seed",
                url_encode_path_segment(&retaindb_agent_id(payload))
            )
        }
        _ => String::new(),
    };
    json!({
        "schema": "hermes_retaindb_live_plan_desktop_v1",
        "method": match tool_name {
            "retaindb_profile" | "retaindb_list_files" | "retaindb_read_file" | "retaindb_agent_model" => "GET",
            "retaindb_forget" | "retaindb_delete_file" => "DELETE",
            _ => "POST",
        },
        "path": path,
        "baseUrl": config["baseUrl"].clone(),
        "project": config["project"].clone(),
        "userId": user_id,
        "sessionId": session_id,
        "networkExecuted": false
    })
}

fn retaindb_live_profile(config: &Value, payload: &Value) -> AppResult<Value> {
    let user_id = retaindb_user_id(payload);
    let primary_path = format!("/v1/memory/profile/{}", url_encode_path_segment(&user_id));
    let primary = retaindb_request(
        config,
        "GET",
        &primary_path,
        Some(&[
            (
                "project",
                retaindb_config_string(config, "project", "default"),
            ),
            ("include_pending", "true".into()),
        ]),
        None,
        Duration::from_secs(8),
    );
    match primary {
        Ok(result) => Ok(json!({
            "request": {"method": "GET", "path": primary_path},
            "fallbackUsed": false,
            "result": result
        })),
        Err(primary_error) => {
            let fallback = retaindb_request(
                config,
                "GET",
                "/v1/memories",
                Some(&[
                    (
                        "project",
                        retaindb_config_string(config, "project", "default"),
                    ),
                    ("user_id", user_id),
                    ("limit", "200".into()),
                ]),
                None,
                Duration::from_secs(8),
            )?;
            Ok(json!({
                "request": {"method": "GET", "path": "/v1/memories", "primaryError": primary_error.to_string()},
                "fallbackUsed": true,
                "result": fallback
            }))
        }
    }
}

fn retaindb_live_search(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("retaindb_search live execution requires query".into())
    })?;
    let top_k = payload
        .get("top_k")
        .or_else(|| payload.get("limit"))
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20);
    let body = json!({
        "project": retaindb_config_string(config, "project", "default"),
        "query": query,
        "user_id": retaindb_user_id(payload),
        "session_id": retaindb_session_id(payload),
        "top_k": top_k,
        "include_pending": true
    });
    let result = retaindb_request(
        config,
        "POST",
        "/v1/memory/search",
        None,
        Some(&body),
        Duration::from_secs(8),
    )?;
    Ok(json!({
        "request": {"method": "POST", "path": "/v1/memory/search", "body": body},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_context(config: &Value, payload: &Value) -> AppResult<Value> {
    let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
        AppError::BadRequest("retaindb_context live execution requires query".into())
    })?;
    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("maxTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(1200)
        .clamp(128, 8000);
    let body = json!({
        "project": retaindb_config_string(config, "project", "default"),
        "query": query,
        "user_id": retaindb_user_id(payload),
        "session_id": retaindb_session_id(payload),
        "include_memories": true,
        "max_tokens": max_tokens
    });
    let result = retaindb_request(
        config,
        "POST",
        "/v1/context/query",
        None,
        Some(&body),
        Duration::from_secs(8),
    )?;
    Ok(json!({
        "request": {"method": "POST", "path": "/v1/context/query", "body": body},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_store(config: &Value, payload: &Value) -> AppResult<Value> {
    let content = string_arg(payload, &["content", "summary", "fact"]).ok_or_else(|| {
        AppError::BadRequest("retaindb_store live execution requires content".into())
    })?;
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "retaindb_store live content cannot be empty".into(),
        ));
    }
    let memory_type = string_arg(payload, &["memory_type", "memoryType", "type"])
        .unwrap_or_else(|| "factual".into());
    let importance = payload
        .get("importance")
        .and_then(Value::as_f64)
        .unwrap_or(0.7)
        .clamp(0.0, 1.0);
    let body = json!({
        "project": retaindb_config_string(config, "project", "default"),
        "content": content.trim(),
        "memory_type": memory_type,
        "user_id": retaindb_user_id(payload),
        "session_id": retaindb_session_id(payload),
        "importance": importance,
        "write_mode": "sync"
    });
    let primary = retaindb_request(
        config,
        "POST",
        "/v1/memory",
        None,
        Some(&body),
        Duration::from_secs(5),
    );
    match primary {
        Ok(result) => Ok(json!({
            "request": {"method": "POST", "path": "/v1/memory", "body": body},
            "fallbackUsed": false,
            "result": result
        })),
        Err(primary_error) => {
            let mut fallback_body = body.clone();
            if let Value::Object(map) = &mut fallback_body {
                map.remove("write_mode");
            }
            let fallback = retaindb_request(
                config,
                "POST",
                "/v1/memories",
                None,
                Some(&fallback_body),
                Duration::from_secs(5),
            )?;
            Ok(json!({
                "request": {"method": "POST", "path": "/v1/memories", "body": fallback_body, "primaryError": primary_error.to_string()},
                "fallbackUsed": true,
                "result": fallback
            }))
        }
    }
}

fn retaindb_live_forget(config: &Value, payload: &Value) -> AppResult<Value> {
    let memory_id = string_arg(payload, &["memory_id", "memoryId", "id"]).ok_or_else(|| {
        AppError::BadRequest("retaindb_forget live execution requires memory_id".into())
    })?;
    let primary_path = format!("/v1/memory/{}", url_encode_path_segment(&memory_id));
    let primary = retaindb_request(
        config,
        "DELETE",
        &primary_path,
        None,
        None,
        Duration::from_secs(5),
    );
    match primary {
        Ok(result) => Ok(json!({
            "request": {"method": "DELETE", "path": primary_path},
            "fallbackUsed": false,
            "result": result
        })),
        Err(primary_error) => {
            let fallback_path = format!("/v1/memories/{}", url_encode_path_segment(&memory_id));
            let fallback = retaindb_request(
                config,
                "DELETE",
                &fallback_path,
                None,
                None,
                Duration::from_secs(5),
            )?;
            Ok(json!({
                "request": {"method": "DELETE", "path": fallback_path, "primaryError": primary_error.to_string()},
                "fallbackUsed": true,
                "result": fallback
            }))
        }
    }
}

fn retaindb_live_list_files(config: &Value, payload: &Value) -> AppResult<Value> {
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200);
    let mut query = vec![("limit", limit.to_string())];
    if let Some(prefix) = string_arg(payload, &["prefix", "pathPrefix"]) {
        if !prefix.trim().is_empty() {
            query.push(("prefix", prefix.trim().to_string()));
        }
    }
    let result = retaindb_request(
        config,
        "GET",
        "/v1/files",
        Some(&query),
        None,
        Duration::from_secs(8),
    )?;
    Ok(json!({
        "request": {"method": "GET", "path": "/v1/files", "query": query},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_read_file(config: &Value, payload: &Value) -> AppResult<Value> {
    let file_id = retaindb_file_id(payload, "retaindb_read_file")?;
    let metadata_path = format!("/v1/files/{}", url_encode_path_segment(&file_id));
    let content_path = format!("/v1/files/{}/content", url_encode_path_segment(&file_id));
    let metadata = retaindb_request(
        config,
        "GET",
        &metadata_path,
        None,
        None,
        Duration::from_secs(8),
    )?;
    let bytes =
        retaindb_request_bytes(config, "GET", &content_path, None, Duration::from_secs(30))?;
    let file_info = metadata.get("file").unwrap_or(&metadata);
    let mime = file_info
        .get("mime_type")
        .or_else(|| file_info.get("mimeType"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = file_info
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let textual = mime.starts_with("text/") || retaindb_filename_looks_textual(name);
    let content = if textual {
        let text = String::from_utf8_lossy(&bytes).to_string();
        Some(json!(text.chars().take(32000).collect::<String>()))
    } else {
        None
    };
    let truncated = textual && String::from_utf8_lossy(&bytes).chars().count() > 32000;
    Ok(json!({
        "request": {"method": "GET", "path": content_path, "metadataPath": metadata_path},
        "fallbackUsed": false,
        "result": {
            "file_id": file_id,
            "metadata": metadata,
            "content": content,
            "contentBytes": bytes.len(),
            "truncated": truncated,
            "note": if textual { Value::Null } else { json!("Binary file - use retaindb_ingest_file to extract text into memory.") }
        }
    }))
}

fn retaindb_live_ingest_file(config: &Value, payload: &Value) -> AppResult<Value> {
    let file_id = retaindb_file_id(payload, "retaindb_ingest_file")?;
    let path = format!("/v1/files/{}/ingest", url_encode_path_segment(&file_id));
    let mut body = serde_json::Map::new();
    if let Some(user_id) = string_arg(payload, &["user_id", "userId", "user"]) {
        body.insert("user_id".into(), json!(user_id));
    }
    if let Some(agent_id) = string_arg(payload, &["agent_id", "agentId", "agent"]) {
        body.insert("agent_id".into(), json!(agent_id));
    }
    let body = Value::Object(body);
    let result = retaindb_request(
        config,
        "POST",
        &path,
        None,
        Some(&body),
        Duration::from_secs(60),
    )?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_delete_file(config: &Value, payload: &Value) -> AppResult<Value> {
    let file_id = retaindb_file_id(payload, "retaindb_delete_file")?;
    let path = format!("/v1/files/{}", url_encode_path_segment(&file_id));
    let result = retaindb_request(config, "DELETE", &path, None, None, Duration::from_secs(5))?;
    Ok(json!({
        "request": {"method": "DELETE", "path": path},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_ingest_session(config: &Value, payload: &Value) -> AppResult<Value> {
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            AppError::BadRequest(
                "retaindb_ingest_session live execution requires messages array".into(),
            )
        })?;
    let body = json!({
        "project": retaindb_config_string(config, "project", "default"),
        "session_id": retaindb_session_id(payload),
        "user_id": retaindb_user_id(payload),
        "messages": messages,
        "write_mode": "sync"
    });
    let result = retaindb_request(
        config,
        "POST",
        "/v1/memory/ingest/session",
        None,
        Some(&body),
        Duration::from_secs(15),
    )?;
    Ok(json!({
        "request": {"method": "POST", "path": "/v1/memory/ingest/session", "body": body},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_agent_model(config: &Value, payload: &Value) -> AppResult<Value> {
    let agent_id = retaindb_agent_id(payload);
    let path = format!(
        "/v1/memory/agent/{}/model",
        url_encode_path_segment(&agent_id)
    );
    let result = retaindb_request(
        config,
        "GET",
        &path,
        Some(&[(
            "project",
            retaindb_config_string(config, "project", "default"),
        )]),
        None,
        Duration::from_secs(4),
    )?;
    Ok(json!({
        "request": {"method": "GET", "path": path},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_seed_agent(config: &Value, payload: &Value) -> AppResult<Value> {
    let content =
        string_arg(payload, &["content", "summary", "soul", "instructions"]).ok_or_else(|| {
            AppError::BadRequest("retaindb_seed_agent live execution requires content".into())
        })?;
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "retaindb_seed_agent live content cannot be empty".into(),
        ));
    }
    let agent_id = retaindb_agent_id(payload);
    let path = format!(
        "/v1/memory/agent/{}/seed",
        url_encode_path_segment(&agent_id)
    );
    let body = json!({
        "project": retaindb_config_string(config, "project", "default"),
        "content": content.trim(),
        "source": string_arg(payload, &["source"]).unwrap_or_else(|| "soul_md".into())
    });
    let result = retaindb_request(
        config,
        "POST",
        &path,
        None,
        Some(&body),
        Duration::from_secs(20),
    )?;
    Ok(json!({
        "request": {"method": "POST", "path": path, "body": body},
        "fallbackUsed": false,
        "result": result
    }))
}

fn retaindb_live_upload_file(config: &Value, payload: &Value) -> AppResult<Value> {
    let local_path =
        string_arg(payload, &["local_path", "localPath", "path"]).ok_or_else(|| {
            AppError::BadRequest("retaindb_upload_file live execution requires local_path".into())
        })?;
    let path = PathBuf::from(local_path.trim());
    if !path.is_file() {
        return Err(AppError::BadRequest(format!(
            "RetainDB upload file not found: {}",
            path.to_string_lossy()
        )));
    }
    let bytes = fs::read(&path)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload.bin")
        .to_string();
    let remote_path = string_arg(payload, &["remote_path", "remotePath"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("/{filename}"));
    let scope = string_arg(payload, &["scope"])
        .unwrap_or_else(|| "PROJECT".into())
        .trim()
        .to_ascii_uppercase();
    let mime = retaindb_guess_mime(&filename);
    let result = retaindb_multipart_upload(config, &bytes, &filename, &remote_path, &scope, &mime)?;
    let mut response = json!({
        "request": {"method": "POST", "path": "/v1/files", "filename": filename, "remotePath": remote_path, "scope": scope, "mimeType": mime, "bytes": bytes.len()},
        "fallbackUsed": false,
        "result": result
    });
    if payload
        .get("ingest")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Some(file_id) = response["result"]
            .get("file")
            .and_then(|file| file.get("id"))
            .or_else(|| response["result"].get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            let ingest_payload = json!({
                "file_id": file_id,
                "user_id": retaindb_user_id(payload),
                "agent_id": retaindb_agent_id(payload)
            });
            response["ingest"] = retaindb_live_ingest_file(config, &ingest_payload)?;
        }
    }
    Ok(response)
}

fn retaindb_request(
    config: &Value,
    method: &str,
    path: &str,
    query: Option<&[(&str, String)]>,
    body: Option<&Value>,
    timeout: Duration,
) -> AppResult<Value> {
    let base_url = retaindb_config_string(config, "baseUrl", "https://api.retaindb.com");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid RETAINDB_BASE_URL: {error}")))?;
    url.set_path(path);
    if let Some(query) = query {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let api_key = env::var("RETAINDB_API_KEY")
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(
            "RETAINDB_API_KEY is required for live RetainDB execution".into(),
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build RetainDB client: {error}"))
        })?;
    let req_method = match method {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "DELETE" => reqwest::Method::DELETE,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported RetainDB method: {other}"
            )))
        }
    };
    let mut request = client
        .request(req_method.clone(), url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header("x-sdk-runtime", "hermes-plugin")
        .header(reqwest::header::ACCEPT, "application/json");
    if path.starts_with("/v1/memory") || path.starts_with("/v1/context") {
        request = request.header("X-API-Key", api_key.clone());
    }
    if !matches!(req_method, reqwest::Method::GET | reqwest::Method::DELETE) {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
    }
    let response = request.send().map_err(|error| {
        AppError::BadRequest(format!("RetainDB {method} {path} failed: {error}"))
    })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "RetainDB {method} {path} failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn retaindb_request_bytes(
    config: &Value,
    method: &str,
    path: &str,
    query: Option<&[(&str, String)]>,
    timeout: Duration,
) -> AppResult<Vec<u8>> {
    let base_url = retaindb_config_string(config, "baseUrl", "https://api.retaindb.com");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid RETAINDB_BASE_URL: {error}")))?;
    url.set_path(path);
    if let Some(query) = query {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let api_key = env::var("RETAINDB_API_KEY")
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(
            "RETAINDB_API_KEY is required for live RetainDB execution".into(),
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build RetainDB client: {error}"))
        })?;
    let req_method = match method {
        "GET" => reqwest::Method::GET,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported RetainDB byte method: {other}"
            )))
        }
    };
    let response = client
        .request(req_method, url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header("x-sdk-runtime", "hermes-plugin")
        .send()
        .map_err(|error| {
            AppError::BadRequest(format!("RetainDB {method} {path} failed: {error}"))
        })?;
    let status = response.status();
    let bytes = response.bytes().map_err(|error| {
        AppError::BadRequest(format!(
            "RetainDB {method} {path} body read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "RetainDB {method} {path} failed ({}): {}",
            status.as_u16(),
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(bytes.to_vec())
}

fn retaindb_multipart_upload(
    config: &Value,
    bytes: &[u8],
    filename: &str,
    remote_path: &str,
    scope: &str,
    mime: &str,
) -> AppResult<Value> {
    let base_url = retaindb_config_string(config, "baseUrl", "https://api.retaindb.com");
    let mut url = reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid RETAINDB_BASE_URL: {error}")))?;
    url.set_path("/v1/files");
    let api_key = env::var("RETAINDB_API_KEY")
        .map(|value| value.trim().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(
            "RETAINDB_API_KEY is required for live RetainDB execution".into(),
        ));
    }
    let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename.to_string())
        .mime_str(mime)
        .map_err(|error| AppError::BadRequest(format!("invalid RetainDB upload MIME: {error}")))?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("file", part)
        .text("path", remote_path.to_string())
        .text("scope", scope.to_string());
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build RetainDB client: {error}"))
        })?;
    let response = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header("x-sdk-runtime", "hermes-plugin")
        .multipart(form)
        .send()
        .map_err(|error| {
            AppError::BadRequest(format!("RetainDB POST /v1/files failed: {error}"))
        })?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "RetainDB POST /v1/files failed ({}): {}",
            status.as_u16(),
            parsed
        )));
    }
    Ok(parsed)
}

fn retaindb_config_string(config: &Value, key: &str, fallback: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn retaindb_user_id(payload: &Value) -> String {
    string_arg(payload, &["user_id", "userId", "user"])
        .unwrap_or_else(|| "default".into())
        .trim()
        .to_string()
}

fn retaindb_session_id(payload: &Value) -> String {
    string_arg(payload, &["session_id", "sessionId", "session"])
        .unwrap_or_else(|| "synthchat".into())
        .trim()
        .to_string()
}

fn retaindb_agent_id(payload: &Value) -> String {
    string_arg(payload, &["agent_id", "agentId", "agent"])
        .unwrap_or_else(|| "hermes".into())
        .trim()
        .to_string()
}

fn retaindb_file_id(payload: &Value, tool: &str) -> AppResult<String> {
    string_arg(payload, &["file_id", "fileId", "id"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest(format!("{tool} live execution requires file_id")))
}

fn retaindb_filename_looks_textual(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    [
        ".txt", ".md", ".json", ".csv", ".yaml", ".yml", ".xml", ".html",
    ]
    .iter()
    .any(|ext| lowered.ends_with(ext))
}

fn retaindb_guess_mime(filename: &str) -> String {
    let lowered = filename.to_ascii_lowercase();
    if lowered.ends_with(".md") {
        "text/markdown".into()
    } else if lowered.ends_with(".json") {
        "application/json".into()
    } else if lowered.ends_with(".csv") {
        "text/csv".into()
    } else if lowered.ends_with(".html") || lowered.ends_with(".htm") {
        "text/html".into()
    } else if lowered.ends_with(".xml") {
        "application/xml".into()
    } else if lowered.ends_with(".zip") {
        "application/zip".into()
    } else if lowered.ends_with(".txt") || lowered.ends_with(".yaml") || lowered.ends_with(".yml") {
        "text/plain".into()
    } else {
        "application/octet-stream".into()
    }
}

fn url_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn byterover_tool(tool_name: &str, payload: &Value) -> AppResult<String> {
    let action = string_arg(payload, &["action", "command"])
        .unwrap_or_else(|| "status".into())
        .trim()
        .to_ascii_lowercase();
    if matches!(tool_name, "brv_query" | "brv_curate" | "brv_status") {
        return byterover_live_tool(tool_name, payload);
    }
    if !matches!(action.as_str(), "" | "status" | "probe" | "run") {
        return Err(AppError::BadRequest(format!(
            "unsupported byterover_status action: {action}"
        )));
    }
    if action == "run" || byterover_live_requested(payload) {
        return byterover_live_tool("brv_status", payload);
    }
    let cwd = byterover_working_dir();
    let cli_candidates = byterover_cli_candidates();
    let cli_path = cli_candidates.iter().find(|path| path.is_file()).cloned();
    let tree_stats = byterover_tree_stats(&cwd);
    let api_key_configured = env::var_os("BRV_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": "byterover_status",
        "provider": "byterover",
        "schema": "hermes_byterover_status_desktop_v1",
        "available": cli_path.is_some(),
        "configured": cli_path.is_some() || cwd.is_dir() || api_key_configured,
        "action": if action.is_empty() { "status" } else { action.as_str() },
        "brvCli": {
            "found": cli_path.is_some(),
            "path": cli_path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "candidates": cli_candidates
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            "installCommands": [
                "npm install -g byterover-cli",
                "curl -fsSL https://byterover.dev/install.sh | sh"
            ],
            "statusCommand": "brv status",
            "queryCommand": "brv query -- <query>",
            "curateCommand": "brv curate -- <content>",
            "executed": false
        },
        "workingDirectory": {
            "path": cwd.to_string_lossy().to_string(),
            "exists": cwd.is_dir(),
            "source": "HERMES_HOME/byterover",
            "profileScoped": true,
            "stats": tree_stats
        },
        "cloudSync": {
            "apiKeyConfigured": api_key_configured,
            "envVar": "BRV_API_KEY",
            "optionalForLocal": true
        },
        "providerContract": {
            "schema": "hermes_byterover_memory_provider_desktop_v1",
            "hermesReference": "plugins/memory/byterover/__init__.py",
            "tools": ["brv_query", "brv_curate", "brv_status"],
            "synthChatToolAlias": "byterover_status",
            "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_pre_compress", "shutdown"],
            "queryTimeoutSeconds": 10,
            "curateTimeoutSeconds": 120,
            "minQueryLength": 10,
            "minOutputLength": 20
        },
        "boundary": "SynthChat exposes ByteRover local CLI and context-tree readiness without running brv by default. Add execute/live/apply:true plus confirmByteRoverLive:true, or byterover_status action=run plus the same confirmation, for confirmed brv CLI execution."
    }))?)
}

fn byterover_live_requested(payload: &Value) -> bool {
    payload_bool(payload, &["execute", "live", "apply"])
}

fn byterover_live_tool(tool_name: &str, payload: &Value) -> AppResult<String> {
    let cwd = byterover_working_dir();
    let cli_candidates = byterover_cli_candidates();
    let cli_path = cli_candidates.iter().find(|path| path.is_file()).cloned();
    let api_key_configured = env::var_os("BRV_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    let planned = byterover_live_plan(tool_name, payload, cli_path.as_ref(), &cwd);
    if !payload_bool(
        payload,
        &[
            "confirmByteRoverLive",
            "confirmLiveByteRover",
            "confirmBrvLive",
        ],
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "byterover",
            "status": "live_confirmation_required",
            "configured": cli_path.is_some() || cwd.is_dir() || api_key_configured,
            "requiredFlag": "confirmByteRoverLive:true",
            "message": "ByteRover live execution runs the local brv CLI and requires execute/live/apply plus confirmByteRoverLive:true.",
            "planned": planned,
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_byterover_provider_desktop_v1",
                "config": byterover_runtime_snapshot(cli_path.as_ref(), &cwd),
                "networkExecuted": false
            }
        }))?);
    }
    let Some(cli_path) = cli_path else {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "tool": tool_name,
            "provider": "byterover",
            "status": "not_available",
            "configured": cwd.is_dir() || api_key_configured,
            "message": "brv CLI was not found. Install ByteRover CLI with npm install -g byterover-cli or curl -fsSL https://byterover.dev/install.sh | sh.",
            "planned": planned,
            "hermesMemoryProviderDesktop": {
                "kind": "hermes_byterover_provider_desktop_v1",
                "config": byterover_runtime_snapshot(None, &cwd),
                "networkExecuted": false
            }
        }))?);
    };
    fs::create_dir_all(&cwd).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to create ByteRover working directory {}: {error}",
            cwd.to_string_lossy()
        ))
    })?;
    let execution = match tool_name {
        "brv_query" => {
            let query = string_arg(payload, &["query", "q"]).ok_or_else(|| {
                AppError::BadRequest("brv_query live execution requires query".into())
            })?;
            if query.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "brv_query live query cannot be empty".into(),
                ));
            }
            let query = query.trim().chars().take(5000).collect::<String>();
            byterover_run_brv(&cli_path, &cwd, &["query", "--", &query], 10)?
        }
        "brv_curate" => {
            let content = string_arg(payload, &["content", "summary", "memory", "fact"])
                .ok_or_else(|| {
                    AppError::BadRequest("brv_curate live execution requires content".into())
                })?;
            if content.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "brv_curate live content cannot be empty".into(),
                ));
            }
            byterover_run_brv(&cli_path, &cwd, &["curate", "--", content.trim()], 120)?
        }
        "brv_status" | "byterover_status" => byterover_run_brv(&cli_path, &cwd, &["status"], 15)?,
        _ => {
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "tool": tool_name,
                "provider": "byterover",
                "status": "unsupported_live_alias",
                "message": "SynthChat live ByteRover execution supports brv_query, brv_curate, brv_status, and byterover_status action=run.",
                "planned": planned,
                "hermesMemoryProviderDesktop": {
                    "kind": "hermes_byterover_provider_desktop_v1",
                    "config": byterover_runtime_snapshot(Some(&cli_path), &cwd),
                    "networkExecuted": false
                }
            }))?)
        }
    };
    let ok = execution["success"].as_bool().unwrap_or(false);
    let status = if ok { "executed" } else { "failed" };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": tool_name,
        "provider": "byterover",
        "status": status,
        "configured": true,
        "result": byterover_result_for_tool(tool_name, &execution),
        "execution": execution,
        "workingDirectory": cwd.to_string_lossy().to_string(),
        "hermesMemoryProviderDesktop": {
            "kind": "hermes_byterover_provider_desktop_v1",
            "config": byterover_runtime_snapshot(Some(&cli_path), &cwd),
            "networkExecuted": true,
            "liveExecution": {
                "schema": "hermes_byterover_live_execution_desktop_v1",
                "confirmed": true,
                "runtime": "brv CLI",
                "supportedAliases": ["brv_query", "brv_curate", "brv_status", "byterover_status"]
            }
        }
    }))?)
}

fn byterover_live_plan(
    tool_name: &str,
    payload: &Value,
    cli_path: Option<&PathBuf>,
    cwd: &PathBuf,
) -> Value {
    let args = match tool_name {
        "brv_query" => vec!["query", "--", "<query>"],
        "brv_curate" => vec!["curate", "--", "<content>"],
        "brv_status" | "byterover_status" => vec!["status"],
        _ => Vec::new(),
    };
    json!({
        "schema": "hermes_byterover_live_plan_desktop_v1",
        "command": cli_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "brv".into()),
        "args": args,
        "cwd": cwd.to_string_lossy().to_string(),
        "timeoutSeconds": match tool_name {
            "brv_curate" => 120,
            "brv_status" | "byterover_status" => 15,
            _ => 10,
        },
        "payloadKeys": payload.as_object().map(|map| map.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "networkExecuted": false
    })
}

fn byterover_runtime_snapshot(cli_path: Option<&PathBuf>, cwd: &PathBuf) -> Value {
    let api_key_configured = env::var_os("BRV_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    json!({
        "schema": "hermes_byterover_config_desktop_v1",
        "cliFound": cli_path.is_some(),
        "cliPath": cli_path.map(|path| path.to_string_lossy().to_string()),
        "workingDirectory": cwd.to_string_lossy().to_string(),
        "workingDirectoryExists": cwd.is_dir(),
        "cloudSyncApiKeyConfigured": api_key_configured,
        "timeouts": {"query": 10, "curate": 120, "status": 15},
        "networkExecuted": false
    })
}

fn byterover_run_brv(
    cli_path: &PathBuf,
    cwd: &PathBuf,
    args: &[&str],
    timeout_seconds: u64,
) -> AppResult<Value> {
    let mut command = Command::new(cli_path);
    command.hide_window();
    let mut child = command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            AppError::BadRequest(format!(
                "failed to run ByteRover CLI {}: {error}",
                cli_path.to_string_lossy()
            ))
        })?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds.max(1));
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| AppError::BadRequest(format!("ByteRover CLI wait failed: {error}")))?
        {
            let output = child.wait_with_output().map_err(|error| {
                AppError::BadRequest(format!("ByteRover CLI output read failed: {error}"))
            })?;
            let stdout_raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr_raw = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = truncate_text_chars(&stdout_raw, 8000);
            let stderr = truncate_text_chars(&stderr_raw, 4000);
            return Ok(json!({
                "success": status.success(),
                "exitCode": status.code(),
                "timedOut": false,
                "command": cli_path.to_string_lossy().to_string(),
                "args": args,
                "cwd": cwd.to_string_lossy().to_string(),
                "timeoutSeconds": timeout_seconds,
                "stdout": stdout,
                "stderr": stderr,
                "stdoutTruncated": stdout_raw.chars().count() > 8000,
                "stderrTruncated": stderr_raw.chars().count() > 4000
            }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().map_err(|error| {
                AppError::BadRequest(format!("ByteRover CLI timeout cleanup failed: {error}"))
            })?;
            let stdout_raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr_raw = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(json!({
                "success": false,
                "exitCode": null,
                "timedOut": true,
                "command": cli_path.to_string_lossy().to_string(),
                "args": args,
                "cwd": cwd.to_string_lossy().to_string(),
                "timeoutSeconds": timeout_seconds,
                "stdout": truncate_text_chars(&stdout_raw, 8000),
                "stderr": if stderr_raw.is_empty() { format!("brv timed out after {timeout_seconds}s") } else { truncate_text_chars(&stderr_raw, 4000) },
                "stdoutTruncated": stdout_raw.chars().count() > 8000,
                "stderrTruncated": stderr_raw.chars().count() > 4000
            }));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn truncate_text_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value.chars().take(max_chars).collect::<String>();
    out.push_str("\n\n[... truncated]");
    out
}

fn byterover_result_for_tool(tool_name: &str, execution: &Value) -> Value {
    let stdout = execution["stdout"].as_str().unwrap_or("").trim();
    let stderr = execution["stderr"].as_str().unwrap_or("").trim();
    let success = execution["success"].as_bool().unwrap_or(false);
    match tool_name {
        "brv_query" => {
            if success && !stdout.is_empty() && stdout.chars().count() >= 20 {
                json!({"result": stdout})
            } else if success {
                json!({"result": "No relevant memories found."})
            } else {
                json!({"error": if stderr.is_empty() { stdout } else { stderr }})
            }
        }
        "brv_curate" => {
            if success {
                json!({"result": "Memory curated successfully.", "output": stdout})
            } else {
                json!({"error": if stderr.is_empty() { stdout } else { stderr }})
            }
        }
        _ => {
            if success {
                json!({"status": stdout})
            } else {
                json!({"error": if stderr.is_empty() { stdout } else { stderr }})
            }
        }
    }
}

fn byterover_working_dir() -> PathBuf {
    hermes_home_dir().join("byterover")
}

fn hermes_home_dir() -> PathBuf {
    env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermes")
        })
}

fn byterover_cli_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            candidates.push(dir.join(if cfg!(windows) { "brv.cmd" } else { "brv" }));
            if cfg!(windows) {
                candidates.push(dir.join("brv.exe"));
                candidates.push(dir.join("brv.ps1"));
            }
        }
    }
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        let home = PathBuf::from(home);
        candidates.push(home.join(".brv-cli").join("bin").join(if cfg!(windows) {
            "brv.cmd"
        } else {
            "brv"
        }));
        candidates.push(home.join(".npm-global").join("bin").join(if cfg!(windows) {
            "brv.cmd"
        } else {
            "brv"
        }));
    }
    if !cfg!(windows) {
        candidates.push(PathBuf::from("/usr/local/bin/brv"));
    }
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_string()))
        .collect()
}

fn byterover_tree_stats(root: &PathBuf) -> Value {
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    let mut markdown_count = 0usize;
    let mut json_count = 0usize;
    let mut total_bytes = 0u64;
    collect_byterover_tree_stats(
        root,
        0,
        &mut file_count,
        &mut dir_count,
        &mut markdown_count,
        &mut json_count,
        &mut total_bytes,
    );
    json!({
        "files": file_count,
        "directories": dir_count,
        "markdownFiles": markdown_count,
        "jsonFiles": json_count,
        "totalBytes": total_bytes
    })
}

fn collect_byterover_tree_stats(
    path: &PathBuf,
    depth: usize,
    file_count: &mut usize,
    dir_count: &mut usize,
    markdown_count: &mut usize,
    json_count: &mut usize,
    total_bytes: &mut u64,
) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            *dir_count += 1;
            collect_byterover_tree_stats(
                &path,
                depth + 1,
                file_count,
                dir_count,
                markdown_count,
                json_count,
                total_bytes,
            );
        } else if metadata.is_file() {
            *file_count += 1;
            *total_bytes = total_bytes.saturating_add(metadata.len());
            let ext = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "md" | "markdown") {
                *markdown_count += 1;
            } else if ext == "json" {
                *json_count += 1;
            }
        }
    }
}

fn normalize_memory_payload(payload: &Value) -> Value {
    let mut normalized = payload.as_object().cloned().unwrap_or_default();
    let action = normalized
        .get("action")
        .or_else(|| normalized.get("operation"))
        .or_else(|| normalized.get("mode"))
        .and_then(Value::as_str)
        .map(normalize_memory_action)
        .unwrap_or_else(|| infer_memory_action(payload));
    normalized.insert("action".into(), json!(action));
    if !normalized.contains_key("summary") {
        if let Some(value) = payload
            .get("fact")
            .or_else(|| payload.get("memory"))
            .or_else(|| payload.get("content"))
            .and_then(Value::as_str)
        {
            normalized.insert("summary".into(), json!(value));
        }
    }
    if !normalized.contains_key("query") {
        if let Some(value) = payload
            .get("q")
            .or_else(|| payload.get("search"))
            .and_then(Value::as_str)
        {
            normalized.insert("query".into(), json!(value));
        }
    }
    normalized.into()
}

fn normalize_memory_action(action: &str) -> String {
    match action
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "search" | "recall" | "find" | "list" | "get" => "read".into(),
        "remember" | "save" | "create" | "insert" => "add".into(),
        "update" | "edit" => "replace".into(),
        "delete" | "forget" => "remove".into(),
        "add" | "read" | "replace" | "remove" => action.trim().to_ascii_lowercase(),
        _ => "read".into(),
    }
}

fn infer_memory_action(payload: &Value) -> String {
    if payload.get("id").is_some()
        && (payload
            .get("remove")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || payload
                .get("delete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || payload
                .get("forget")
                .and_then(Value::as_bool)
                .unwrap_or(false))
    {
        return "remove".into();
    }
    if payload.get("id").is_some()
        && (payload.get("summary").is_some()
            || payload.get("fact").is_some()
            || payload.get("memory").is_some()
            || payload.get("content").is_some())
    {
        return "replace".into();
    }
    if payload.get("summary").is_some()
        || payload.get("fact").is_some()
        || payload.get("memory").is_some()
        || payload.get("content").is_some()
    {
        return "add".into();
    }
    "read".into()
}

pub(super) fn execute_manage_memory(
    store: &AppStore,
    persona: &Persona,
    payload: &Value,
) -> AppResult<(String, Value, bool)> {
    execute_manage_memory_for_run(store, persona, "", payload)
}

pub(super) fn execute_manage_memory_for_run(
    store: &AppStore,
    persona: &Persona,
    run_id: &str,
    payload: &Value,
) -> AppResult<(String, Value, bool)> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("read")
        .trim()
        .to_lowercase();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20) as usize;
    let target = memory_target_from_payload(payload)?;
    let memories = store
        .memories(Some(&persona.id))?
        .into_iter()
        .filter(|memory| memory.target.trim().is_empty() || memory.target == target)
        .collect::<Vec<_>>();
    match action.as_str() {
        "read" | "list" | "search" => {
            let query = payload
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let mut ranked = memories
                .into_iter()
                .filter(|memory| crate::store::scan_memory_content(&memory.summary).is_none())
                .filter_map(|memory| {
                    let score = if query.is_empty() {
                        memory.importance as u32
                    } else {
                        memory_relevance_score(&memory.summary, &query)
                    };
                    (score > 0).then_some((score, memory))
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_score, left), (right_score, right)| {
                right_score
                    .cmp(left_score)
                    .then_with(|| right.importance.cmp(&left.importance))
                    .then_with(|| right.updated_at.cmp(&left.updated_at))
            });
            let mut memories = ranked
                .into_iter()
                .map(|(_, memory)| memory)
                .collect::<Vec<_>>();
            memories.truncate(limit);
            let text = if memories.is_empty() {
                if query.is_empty() {
                    format!("No long-term {target} memory is stored for this persona.")
                } else {
                    format!("No long-term {target} memory matched `{query}`.")
                }
            } else {
                memories
                    .iter()
                    .map(|memory| {
                        format!("- {} [{}] {}", memory.id, memory.importance, memory.summary)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok((
                text,
                json!({"action": "read", "target": target, "query": query, "memories": memories}),
                true,
            ))
        }
        "add" | "remember" => {
            let summary = string_arg(payload, &["summary", "content", "fact"])
                .ok_or_else(|| AppError::BadRequest("manage_memory add requires summary".into()))?;
            if summary.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "manage_memory add summary cannot be empty".into(),
                ));
            }
            let importance = payload
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(4)
                .clamp(1, 5) as u8;
            let memory = store.save_memory(MemoryEntry {
                id: String::new(),
                persona_id: persona.id.clone(),
                target: target.clone(),
                summary: summary.trim().to_string(),
                importance,
                created_at: String::new(),
                updated_at: String::new(),
            })?;
            on_memory_write(store, run_id, persona, "add", &memory.id, summary.trim())?;
            Ok((
                format!(
                    "Stored long-term memory: {} [{}] {}",
                    memory.id, memory.importance, memory.summary
                ),
                json!({"action": "add", "target": target, "memoryId": memory.id, "memory": memory}),
                true,
            ))
        }
        "replace" | "update" => {
            let summary =
                string_arg(payload, &["summary", "content", "fact"]).ok_or_else(|| {
                    AppError::BadRequest("manage_memory replace requires summary".into())
                })?;
            let existing = resolve_memory_selector(&memories, payload, "replace")?;
            let importance = payload
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(existing.importance as u64)
                .clamp(1, 5) as u8;
            let memory = store.save_memory(MemoryEntry {
                id: existing.id.clone(),
                persona_id: persona.id.clone(),
                target: target.clone(),
                summary: summary.trim().to_string(),
                importance,
                created_at: existing.created_at.clone(),
                updated_at: String::new(),
            })?;
            on_memory_write(
                store,
                run_id,
                persona,
                "replace",
                &memory.id,
                summary.trim(),
            )?;
            Ok((
                format!(
                    "Replaced long-term memory: {} [{}] {}",
                    memory.id, memory.importance, memory.summary
                ),
                json!({"action": "replace", "target": target, "memoryId": memory.id, "memory": memory}),
                true,
            ))
        }
        "remove" | "delete" | "forget" => {
            let existing = resolve_memory_selector(&memories, payload, "remove")?;
            store.delete_memory(&existing.id)?;
            on_memory_write(
                store,
                run_id,
                persona,
                "remove",
                &existing.id,
                &existing.summary,
            )?;
            Ok((
                format!(
                    "Removed long-term memory: {} {}",
                    existing.id, existing.summary
                ),
                json!({"action": "remove", "target": target, "memoryId": existing.id, "memory": existing}),
                true,
            ))
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported manage_memory action '{other}'. Use read, add, replace, or remove."
        ))),
    }
}

fn resolve_memory_selector<'a>(
    memories: &'a [MemoryEntry],
    payload: &Value,
    action: &str,
) -> AppResult<&'a MemoryEntry> {
    if let Some(id) = string_arg(payload, &["id", "memoryId", "memory_id"]) {
        return memories
            .iter()
            .find(|memory| memory.id == id)
            .ok_or_else(|| AppError::BadRequest(format!("memory not found: {id}")));
    }
    let needle = string_arg(
        payload,
        &[
            "oldText", "old_text", "match", "selector", "query", "text", "contains",
        ],
    )
    .ok_or_else(|| {
        AppError::BadRequest(format!(
            "manage_memory {action} requires id or oldText substring"
        ))
    })?;
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(AppError::BadRequest(format!(
            "manage_memory {action} substring cannot be empty"
        )));
    }
    let matches = memories
        .iter()
        .filter(|memory| memory.summary.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(AppError::BadRequest(format!(
            "memory not found for substring: {needle}"
        ))),
        _ => Err(AppError::BadRequest(format!(
            "memory substring is ambiguous ({}) matches: {needle}",
            matches.len()
        ))),
    }
}

fn memory_target_from_payload(payload: &Value) -> AppResult<String> {
    let target = payload
        .get("target")
        .or_else(|| payload.get("store"))
        .or_else(|| payload.get("scope"))
        .and_then(Value::as_str)
        .unwrap_or("memory")
        .trim()
        .to_ascii_lowercase();
    match target.as_str() {
        "" | "memory" | "agent" | "assistant" => Ok("memory".into()),
        "user" | "profile" => Ok("user".into()),
        other => Err(AppError::BadRequest(format!(
            "invalid memory target '{other}'. Use 'memory' or 'user'."
        ))),
    }
}

fn memory_relevance_score(text: &str, query: &str) -> u32 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 1;
    }
    let text = text.to_lowercase();
    if text.contains(&query) {
        return 100 + query.len() as u32;
    }
    query
        .split_whitespace()
        .filter(|term| !term.is_empty() && text.contains(*term))
        .map(|term| 10 + term.len() as u32)
        .sum()
}

fn active_memory_provider() -> String {
    std::env::var("SYNTHCHAT_MEMORY_PROVIDER")
        .or_else(|_| std::env::var("HERMES_MEMORY_PROVIDER"))
        .or_else(|_| std::env::var("MEMORY_PROVIDER"))
        .unwrap_or_else(|_| "builtin".into())
        .trim()
        .to_ascii_lowercase()
}

fn hermes_memory_providers(store: &AppStore) -> Vec<Value> {
    vec![
        json!({
            "name": "builtin",
            "description": "SynthChat built-in long-term persona memory.",
            "available": true,
            "tools": ["memory", "recall_memory", "remember_fact", "manage_memory"],
            "hooks": ["on_memory_write", "turn_prefetch", "pre_compress"],
            "source": "native"
        }),
        json!({
            "name": "holographic",
            "description": "Hermes holographic memory adaptation: local structured fact store with entity lookup, trust scoring, and feedback.",
            "available": true,
            "tools": ["fact_store", "fact_feedback"],
            "hooks": ["on_memory_write"],
            "source": "native_desktop",
            "statePath": holographic_facts_path(store)
        }),
        supermemory_provider_status(),
        honcho_provider_status(),
        mem0_provider_status(),
        openviking_provider_status(),
        byterover_provider_status(),
        hindsight_provider_status(),
        retaindb_provider_status(),
    ]
}

fn mem0_provider_status() -> Value {
    let config = mem0_config_snapshot();
    let configured = config["apiKeyConfigured"].as_bool().unwrap_or(false);
    json!({
        "name": "mem0",
        "description": "Mem0 server-side LLM fact extraction with semantic search, reranking, and automatic deduplication.",
        "available": configured,
        "configured": configured,
        "requiredEnv": ["MEM0_API_KEY"],
        "tools": ["mem0_profile", "mem0_search", "mem0_conclude"],
        "hooks": ["sync_turn", "queue_prefetch", "shutdown"],
        "source": "hermes_mem0_provider_desktop_v1",
        "config": config
    })
}

fn openviking_provider_status() -> Value {
    let config = openviking_config_snapshot();
    let configured = config["configured"].as_bool().unwrap_or(false);
    json!({
        "name": "openviking",
        "description": "OpenViking context database with session-managed extraction, tiered retrieval, viking:// filesystem browsing, and resource ingestion.",
        "available": configured,
        "configured": configured,
        "requiredEnv": ["OPENVIKING_ENDPOINT"],
        "optionalEnv": ["OPENVIKING_API_KEY", "OPENVIKING_ACCOUNT", "OPENVIKING_USER", "OPENVIKING_AGENT"],
        "tools": ["viking_search", "viking_read", "viking_browse", "viking_remember", "viking_add_resource"],
        "hooks": ["queue_prefetch", "prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown", "atexit_commit"],
        "source": "hermes_openviking_provider_desktop_v1",
        "config": config,
        "restApi": openviking_rest_contract()
    })
}

fn openviking_config_snapshot() -> Value {
    let endpoint_env_present = env::var_os("OPENVIKING_ENDPOINT")
        .filter(|value| !value.is_empty())
        .is_some();
    let endpoint = env::var("OPENVIKING_ENDPOINT")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:1933".into());
    let api_key_configured = env::var_os("OPENVIKING_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    let account = env::var("OPENVIKING_ACCOUNT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into());
    let user = env::var("OPENVIKING_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into());
    let agent = env::var("OPENVIKING_AGENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hermes".into());
    let local_dev_mode = !api_key_configured && openviking_endpoint_is_loopback(&endpoint);
    json!({
        "schema": "hermes_openviking_config_desktop_v1",
        "configured": endpoint_env_present || local_dev_mode,
        "endpoint": endpoint,
        "endpointSource": if endpoint_env_present { "env" } else { "default" },
        "defaultEndpoint": "http://127.0.0.1:1933",
        "apiKeyConfigured": api_key_configured,
        "apiKeyOptionalForLocalDev": true,
        "localDevMode": local_dev_mode,
        "account": account,
        "user": user,
        "agent": agent,
        "timeoutSeconds": 30.0,
        "tenantHeaders": {
            "X-OpenViking-Account": account,
            "X-OpenViking-User": user,
            "X-OpenViking-Agent": agent,
            "Authorization": if api_key_configured { "Bearer <redacted>" } else { "" },
            "X-API-Key": if api_key_configured { "<redacted>" } else { "" }
        },
        "memoryWriteUriTemplate": format!("viking://user/{user}/memories/{{subdir}}/mem_<uuid12>.md"),
        "memoryWriteTargetSubdirs": {
            "user": "preferences",
            "memory": "patterns",
            "default": "preferences"
        },
        "rememberCategorySubdirs": {
            "preference": "preferences",
            "entity": "entities",
            "event": "events",
            "case": "cases",
            "pattern": "patterns",
            "default": "preferences"
        },
        "remoteResourcePrefixes": ["http://", "https://", "git@", "ssh://", "git://"],
        "directoryUpload": {
            "zipBeforeUpload": true,
            "tempUploadEndpoint": "/api/v1/resources/temp_upload"
        },
        "networkExecuted": false,
        "envVars": ["OPENVIKING_ENDPOINT", "OPENVIKING_API_KEY", "OPENVIKING_ACCOUNT", "OPENVIKING_USER", "OPENVIKING_AGENT"]
    })
}

fn openviking_endpoint_is_loopback(endpoint: &str) -> bool {
    let lowered = endpoint.to_ascii_lowercase();
    lowered.contains("127.0.0.1")
        || lowered.contains("localhost")
        || lowered.contains("[::1]")
        || lowered.contains("://::1")
}

fn openviking_rest_contract() -> Value {
    json!({
        "schema": "hermes_openviking_rest_contract_desktop_v1",
        "health": "GET /health",
        "search": "POST /api/v1/search/find",
        "sessionMessage": "POST /api/v1/sessions/{session_id}/messages",
        "sessionCommit": "POST /api/v1/sessions/{session_id}/commit",
        "contentRead": "GET /api/v1/content/read?uri=...",
        "contentAbstract": "GET /api/v1/content/abstract?uri=...",
        "contentOverview": "GET /api/v1/content/overview?uri=...",
        "contentWrite": "POST /api/v1/content/write",
        "fsList": "GET /api/v1/fs/ls?uri=...",
        "fsTree": "GET /api/v1/fs/tree?uri=...",
        "fsStat": "GET /api/v1/fs/stat?uri=...",
        "resourceCreate": "POST /api/v1/resources",
        "tempUpload": "POST /api/v1/resources/temp_upload",
        "summaryLevels": ["abstract", "overview", "full"],
        "searchModes": ["auto", "fast", "deep"],
        "memoryExtractionCategories": ["profile", "preferences", "entities", "events", "cases", "patterns"],
        "networkExecuted": false
    })
}

fn hindsight_provider_status() -> Value {
    let config = hindsight_config_snapshot();
    let configured = config["configured"].as_bool().unwrap_or(false);
    json!({
        "name": "hindsight",
        "description": "Hindsight long-term memory with knowledge graph, entity resolution, observations, multi-strategy recall, and cross-memory reflection.",
        "available": configured,
        "configured": configured,
        "requiredEnv": [],
        "optionalEnv": ["HINDSIGHT_API_KEY", "HINDSIGHT_BANK_ID", "HINDSIGHT_BUDGET", "HINDSIGHT_API_URL", "HINDSIGHT_MODE", "HINDSIGHT_TIMEOUT", "HINDSIGHT_IDLE_TIMEOUT", "HINDSIGHT_LLM_API_KEY"],
        "tools": ["hindsight_retain", "hindsight_recall", "hindsight_reflect"],
        "synthChatAliases": ["hindsight_remember", "hindsight_search", "hindsight_reflect"],
        "hooks": ["prefetch", "sync_turn", "on_session_end", "on_session_switch", "shutdown", "atexit_shutdown"],
        "source": "hermes_hindsight_provider_desktop_v1",
        "config": config,
        "apiContract": hindsight_api_contract()
    })
}

fn hindsight_config_snapshot() -> Value {
    let profile_path = hermes_home_dir().join("hindsight").join("config.json");
    let legacy_path = hindsight_legacy_config_path();
    let profile_config = read_json_object(&profile_path);
    let legacy_config = if profile_config.is_some() {
        None
    } else {
        read_json_object(&legacy_path)
    };
    let config = profile_config.clone().or(legacy_config.clone());
    let config_source = if profile_config.is_some() {
        "profile"
    } else if legacy_config.is_some() {
        "legacy"
    } else {
        "env"
    };
    let mode = normalize_hindsight_mode(&hindsight_string(&config, "mode").unwrap_or_else(|| {
        env::var("HINDSIGHT_MODE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "cloud".into())
    }));
    let api_key_configured = hindsight_string(&config, "apiKey")
        .or_else(|| hindsight_string(&config, "api_key"))
        .or_else(|| env::var("HINDSIGHT_API_KEY").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let default_api_url = if matches!(mode.as_str(), "local_embedded" | "local_external") {
        "http://localhost:8888"
    } else {
        "https://api.hindsight.vectorize.io"
    };
    let api_url = hindsight_string(&config, "api_url")
        .or_else(|| env::var("HINDSIGHT_API_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_api_url.into());
    let timeout = hindsight_i64(&config, "timeout")
        .or_else(|| {
            env::var("HINDSIGHT_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(120);
    let idle_timeout = hindsight_i64(&config, "idle_timeout")
        .or_else(|| {
            env::var("HINDSIGHT_IDLE_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(300);
    let banks = config
        .as_ref()
        .and_then(|value| value.get("banks"))
        .and_then(|value| value.get("hermes"));
    let static_bank_id = hindsight_string(&config, "bank_id")
        .or_else(|| {
            banks
                .and_then(|value| value.get("bankId"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| env::var("HINDSIGHT_BANK_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hermes".into());
    let bank_template = hindsight_string(&config, "bank_id_template").unwrap_or_default();
    let resolved_bank_id = resolve_hindsight_bank_template(
        &bank_template,
        &static_bank_id,
        &[
            ("profile", "default"),
            ("workspace", ""),
            ("platform", ""),
            ("user", ""),
            ("session", ""),
        ],
    );
    let budget = normalize_hindsight_budget(
        &hindsight_string(&config, "recall_budget")
            .or_else(|| hindsight_string(&config, "budget"))
            .or_else(|| {
                banks
                    .and_then(|value| value.get("budget"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .or_else(|| env::var("HINDSIGHT_BUDGET").ok())
            .unwrap_or_else(|| "mid".into()),
    );
    let memory_mode = normalize_hindsight_memory_mode(
        &hindsight_string(&config, "memory_mode").unwrap_or_else(|| "hybrid".into()),
    );
    let prefetch_method = match hindsight_string(&config, "recall_prefetch_method")
        .or_else(|| hindsight_string(&config, "prefetch_method"))
        .unwrap_or_else(|| "recall".into())
        .trim()
    {
        "reflect" => "reflect",
        _ => "recall",
    };
    let retain_tags = hindsight_tags(
        hindsight_raw(&config, "retain_tags")
            .cloned()
            .or_else(|| env::var("HINDSIGHT_RETAIN_TAGS").ok().map(Value::String)),
    );
    let recall_tags = hindsight_tags(hindsight_raw(&config, "recall_tags").cloned());
    let recall_types = hindsight_tags(hindsight_raw(&config, "recall_types").cloned());
    let llm_provider = hindsight_string(&config, "llm_provider").unwrap_or_default();
    let llm_model = hindsight_string(&config, "llm_model").unwrap_or_default();
    let llm_base_url = hindsight_string(&config, "llm_base_url")
        .or_else(|| env::var("HINDSIGHT_API_LLM_BASE_URL").ok())
        .unwrap_or_default();
    let llm_api_key_configured = hindsight_string(&config, "llmApiKey")
        .or_else(|| hindsight_string(&config, "llm_api_key"))
        .or_else(|| env::var("HINDSIGHT_LLM_API_KEY").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let mut out = serde_json::Map::new();
    out.insert("schema".into(), json!("hermes_hindsight_config_desktop_v1"));
    out.insert("configSource".into(), json!(config_source));
    out.insert(
        "profileConfigPath".into(),
        json!(profile_path.to_string_lossy().to_string()),
    );
    out.insert("profileConfigExists".into(), json!(profile_path.is_file()));
    out.insert(
        "legacyConfigPath".into(),
        json!(legacy_path.to_string_lossy().to_string()),
    );
    out.insert("legacyConfigExists".into(), json!(legacy_path.is_file()));
    out.insert(
        "configured".into(),
        json!(api_key_configured || mode != "cloud"),
    );
    out.insert("mode".into(), json!(mode));
    out.insert("apiUrl".into(), json!(api_url));
    out.insert("defaultApiUrl".into(), json!(default_api_url));
    out.insert("apiKeyConfigured".into(), json!(api_key_configured));
    out.insert("minimumClientVersion".into(), json!("0.4.22"));
    out.insert("appendUpdateModeMinVersion".into(), json!("0.5.0"));
    out.insert("appendCapabilityProbe".into(), json!({"endpoint": "/version", "executed": false, "fallback": "per-process document_id without update_mode"}));
    out.insert("timeoutSeconds".into(), json!(timeout));
    out.insert("idleTimeoutSeconds".into(), json!(idle_timeout));
    out.insert("bankId".into(), json!(resolved_bank_id));
    out.insert("staticBankId".into(), json!(static_bank_id));
    out.insert("bankIdTemplate".into(), json!(bank_template));
    out.insert("budget".into(), json!(budget));
    out.insert("memoryMode".into(), json!(memory_mode));
    out.insert("prefetchMethod".into(), json!(prefetch_method));
    out.insert("retainTags".into(), json!(retain_tags));
    out.insert(
        "retainSource".into(),
        json!(hindsight_string(&config, "retain_source")
            .or_else(|| env::var("HINDSIGHT_RETAIN_SOURCE").ok())
            .unwrap_or_default()),
    );
    out.insert(
        "retainUserPrefix".into(),
        json!(hindsight_string(&config, "retain_user_prefix")
            .or_else(|| env::var("HINDSIGHT_RETAIN_USER_PREFIX").ok())
            .unwrap_or_else(|| "User".into())),
    );
    out.insert(
        "retainAssistantPrefix".into(),
        json!(hindsight_string(&config, "retain_assistant_prefix")
            .or_else(|| env::var("HINDSIGHT_RETAIN_ASSISTANT_PREFIX").ok())
            .unwrap_or_else(|| "Assistant".into())),
    );
    out.insert(
        "autoRetain".into(),
        json!(hindsight_bool(&config, "auto_retain").unwrap_or(true)),
    );
    out.insert(
        "retainEveryNTurns".into(),
        json!(hindsight_i64(&config, "retain_every_n_turns")
            .unwrap_or(1)
            .max(1)),
    );
    out.insert(
        "retainAsync".into(),
        json!(hindsight_bool(&config, "retain_async").unwrap_or(true)),
    );
    out.insert(
        "retainContext".into(),
        json!(hindsight_string(&config, "retain_context")
            .unwrap_or_else(|| "conversation between Hermes Agent and the User".into())),
    );
    out.insert(
        "autoRecall".into(),
        json!(hindsight_bool(&config, "auto_recall").unwrap_or(true)),
    );
    out.insert(
        "recallMaxTokens".into(),
        json!(hindsight_i64(&config, "recall_max_tokens").unwrap_or(4096)),
    );
    out.insert(
        "recallMaxInputChars".into(),
        json!(hindsight_i64(&config, "recall_max_input_chars").unwrap_or(800)),
    );
    out.insert(
        "recallPromptPreambleConfigured".into(),
        json!(hindsight_string(&config, "recall_prompt_preamble").is_some()),
    );
    out.insert("recallTags".into(), json!(recall_tags));
    out.insert(
        "recallTagsMatch".into(),
        json!(hindsight_string(&config, "recall_tags_match").unwrap_or_else(|| "any".into())),
    );
    out.insert(
        "recallTypes".into(),
        json!(if recall_types.is_empty() {
            vec!["observation".to_string()]
        } else {
            recall_types
        }),
    );
    out.insert(
        "bankMissionConfigured".into(),
        json!(hindsight_string(&config, "bank_mission").is_some()),
    );
    out.insert(
        "bankRetainMissionConfigured".into(),
        json!(hindsight_string(&config, "bank_retain_mission").is_some()),
    );
    out.insert("localEmbedded".into(), json!({
        "profile": hindsight_string(&config, "profile").unwrap_or_else(|| "hermes".into()),
        "profileEnvPath": hindsight_embedded_profile_env_path(&config).to_string_lossy().to_string(),
        "llmProvider": llm_provider,
        "daemonProvider": if matches!(llm_provider.as_str(), "openai_compatible" | "openrouter") { "openai" } else { llm_provider.as_str() },
        "llmModel": llm_model,
        "llmBaseUrl": llm_base_url,
        "llmApiKeyConfigured": llm_api_key_configured,
        "idleTimeoutSeconds": idle_timeout,
        "runtimeImportProbeExecuted": false
    }));
    out.insert("sessionLifecycle".into(), json!({
        "documentIdStrategy": "per-process unique document id; reuse session id with update_mode=append only after /version >= 0.5.0 probe",
        "flushOnSessionSwitch": true,
        "lineageTags": ["session:<id>", "parent:<id>"],
        "writerQueue": true,
        "sharedAsyncLoop": true,
        "atexitShutdown": true
    }));
    out.insert(
        "envVars".into(),
        json!([
            "HINDSIGHT_API_KEY",
            "HINDSIGHT_BANK_ID",
            "HINDSIGHT_BUDGET",
            "HINDSIGHT_API_URL",
            "HINDSIGHT_MODE",
            "HINDSIGHT_TIMEOUT",
            "HINDSIGHT_IDLE_TIMEOUT",
            "HINDSIGHT_RETAIN_TAGS",
            "HINDSIGHT_RETAIN_SOURCE",
            "HINDSIGHT_RETAIN_USER_PREFIX",
            "HINDSIGHT_RETAIN_ASSISTANT_PREFIX",
            "HINDSIGHT_LLM_API_KEY",
            "HINDSIGHT_API_LLM_BASE_URL"
        ]),
    );
    out.insert("networkExecuted".into(), json!(false));
    Value::Object(out)
}

fn hindsight_api_contract() -> Value {
    json!({
        "schema": "hermes_hindsight_api_contract_desktop_v1",
        "clientPackage": "hindsight-client>=0.4.22",
        "cloudDefaultApiUrl": "https://api.hindsight.vectorize.io",
        "localDefaultApiUrl": "http://localhost:8888",
        "tools": {
            "hindsight_retain": "client.aretain(bank_id, content, context, tags, metadata)",
            "hindsight_recall": "client.arecall(bank_id, query, budget, max_tokens, tags, tags_match, types)",
            "hindsight_reflect": "client.areflect(bank_id, query, budget)"
        },
        "batchRetain": "client.aretain_batch(bank_id, items, document_id, retain_async)",
        "appendCapabilityProbe": "GET {api_url}/version; update_mode=append requires >=0.5.0",
        "memoryModes": ["hybrid", "context", "tools"],
        "budgets": ["low", "mid", "high"],
        "modes": ["cloud", "local_embedded", "local_external"],
        "networkExecuted": false
    })
}

fn retaindb_provider_status() -> Value {
    let config = retaindb_config_snapshot();
    let configured = config["configured"].as_bool().unwrap_or(false);
    json!({
        "name": "retaindb",
        "description": "RetainDB cloud memory with durable write-behind ingest, semantic search, profile/context retrieval, dialectic synthesis, agent self-model, and shared file store tools.",
        "available": configured,
        "configured": configured,
        "requiredEnv": ["RETAINDB_API_KEY"],
        "optionalEnv": ["RETAINDB_BASE_URL", "RETAINDB_PROJECT"],
        "tools": ["retaindb_profile", "retaindb_search", "retaindb_context", "retaindb_remember", "retaindb_forget", "retaindb_upload_file", "retaindb_list_files", "retaindb_read_file", "retaindb_ingest_file", "retaindb_delete_file"],
        "synthChatAliases": ["retaindb_profile", "retaindb_search", "retaindb_store"],
        "hooks": ["queue_prefetch", "prefetch", "sync_turn", "on_memory_write", "shutdown"],
        "source": "hermes_retaindb_provider_desktop_v1",
        "config": config,
        "apiContract": retaindb_api_contract()
    })
}

fn retaindb_config_snapshot() -> Value {
    let api_key_configured = env::var_os("RETAINDB_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    let base_url = env::var("RETAINDB_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.retaindb.com".into());
    let explicit_project = env::var("RETAINDB_PROJECT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let project = explicit_project.clone().unwrap_or_else(|| "default".into());
    json!({
        "schema": "hermes_retaindb_config_desktop_v1",
        "configured": api_key_configured,
        "apiKeyConfigured": api_key_configured,
        "baseUrl": base_url,
        "baseUrlSource": if env::var_os("RETAINDB_BASE_URL").filter(|value| !value.is_empty()).is_some() { "env" } else { "default" },
        "defaultBaseUrl": "https://api.retaindb.com",
        "project": project,
        "projectSource": if explicit_project.is_some() { "env" } else { "default" },
        "projectFallback": "default or hermes-<profile> during Hermes initialize when no RETAINDB_PROJECT is set",
        "queueDbPath": hermes_home_dir().join("retaindb_queue.db").to_string_lossy().to_string(),
        "writeBehindQueue": {
            "durable": true,
            "storage": "SQLite pending table",
            "replayOnStartup": true,
            "shutdownSentinel": true
        },
        "prefetch": {
            "contextQuery": true,
            "dialecticAskUser": true,
            "agentSelfModel": true,
            "threadJoinTimeoutSeconds": 2.0
        },
        "memoryTypes": ["factual", "preference", "goal", "instruction", "event", "opinion"],
        "fileStore": {
            "scopes": ["USER", "PROJECT", "ORG"],
            "rdbUri": "rdb://...",
            "binaryReadBoundary": true,
            "maxTextReadChars": 32000
        },
        "requestHeaders": {
            "Authorization": if api_key_configured { "Bearer <redacted>" } else { "" },
            "X-API-KeyForMemoryRoutes": if api_key_configured { "<redacted>" } else { "" },
            "x-sdk-runtime": "hermes-plugin"
        },
        "networkExecuted": false,
        "envVars": ["RETAINDB_API_KEY", "RETAINDB_BASE_URL", "RETAINDB_PROJECT"]
    })
}

fn retaindb_api_contract() -> Value {
    json!({
        "schema": "hermes_retaindb_api_contract_desktop_v1",
        "memory": {
            "queryContext": "POST /v1/context/query",
            "search": "POST /v1/memory/search",
            "profile": "GET /v1/memory/profile/{user_id}; fallback GET /v1/memories",
            "addMemory": "POST /v1/memory; fallback POST /v1/memories",
            "deleteMemory": "DELETE /v1/memory/{memory_id}; fallback DELETE /v1/memories/{memory_id}",
            "ingestSession": "POST /v1/memory/ingest/session",
            "askUser": "POST /v1/memory/profile/{user_id}/ask",
            "agentModel": "GET /v1/memory/agent/{agent_id}/model",
            "seedAgentIdentity": "POST /v1/memory/agent/{agent_id}/seed"
        },
        "files": {
            "upload": "POST /v1/files multipart",
            "list": "GET /v1/files",
            "metadata": "GET /v1/files/{file_id}",
            "content": "GET /v1/files/{file_id}/content",
            "ingest": "POST /v1/files/{file_id}/ingest",
            "delete": "DELETE /v1/files/{file_id}"
        },
        "searchTopK": {"default": 8, "max": 20},
        "contextMaxTokens": 1200,
        "memoryImportanceDefault": 0.7,
        "networkExecuted": false
    })
}

fn honcho_provider_status() -> Value {
    let config = honcho_config_snapshot();
    let configured = config["configured"].as_bool().unwrap_or(false);
    json!({
        "name": "honcho",
        "description": "Honcho AI-native memory with cross-session user modeling, dialectic Q&A, semantic search, peer cards, and conclusions.",
        "available": configured,
        "configured": configured,
        "requiredEnv": ["HONCHO_API_KEY", "HONCHO_BASE_URL"],
        "optionalEnv": ["HONCHO_ENVIRONMENT", "HONCHO_TIMEOUT", "HERMES_HONCHO_HOST"],
        "tools": ["honcho_profile", "honcho_search", "honcho_reasoning", "honcho_context", "honcho_conclude"],
        "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown"],
        "source": "hermes_honcho_provider_desktop_v1",
        "config": config
    })
}

fn honcho_config_snapshot() -> Value {
    let config_path = hermes_home_dir().join("honcho.json");
    let file_config = read_json_object(&config_path);
    let host = env::var("HERMES_HONCHO_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hermes".into());
    let host_config = honcho_host_block(&file_config, &host);
    let api_key_env = env::var("HONCHO_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base_url_env = env::var("HONCHO_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let environment_env = env::var("HONCHO_ENVIRONMENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let timeout_env = env::var("HONCHO_TIMEOUT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let api_key = honcho_string(&file_config, &host_config, "apiKey").or(api_key_env);
    let base_url = honcho_string(&file_config, &host_config, "baseUrl")
        .or_else(|| honcho_string(&file_config, &host_config, "base_url"))
        .or(base_url_env);
    let configured = api_key.is_some() || base_url.is_some();
    let enabled = honcho_bool(&file_config, &host_config, "enabled").unwrap_or(configured);
    let explicitly_configured = host_config.is_some()
        || file_config
            .as_ref()
            .and_then(|value| value.get("enabled"))
            .is_some();
    let observation_mode_default = if explicitly_configured {
        "unified"
    } else {
        "directional"
    };
    let observation_mode = normalize_honcho_observation_mode(
        &honcho_string(&file_config, &host_config, "observationMode")
            .unwrap_or_else(|| observation_mode_default.into()),
    );
    let observation = honcho_observation_snapshot(&file_config, &host_config, &observation_mode);
    let dialectic_depth = honcho_i64(&file_config, &host_config, "dialecticDepth")
        .unwrap_or(1)
        .clamp(1, 3);
    let dialectic_depth_levels =
        honcho_dialectic_depth_levels(&file_config, &host_config, dialectic_depth as usize);
    let reasoning_level = normalize_honcho_reasoning_level(
        &honcho_string(&file_config, &host_config, "dialecticReasoningLevel")
            .unwrap_or_else(|| "low".into()),
        "low",
        true,
    );
    let reasoning_level_cap = normalize_honcho_reasoning_level(
        &honcho_string(&file_config, &host_config, "reasoningLevelCap")
            .unwrap_or_else(|| "high".into()),
        "high",
        false,
    );
    let timeout = honcho_f64(&file_config, &host_config, "timeout")
        .or_else(|| honcho_f64(&file_config, &host_config, "requestTimeout"))
        .or_else(|| timeout_env.and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| *value > 0.0)
        .unwrap_or(30.0);
    let api_key_source = if honcho_string(&file_config, &host_config, "apiKey").is_some() {
        "file"
    } else if env::var_os("HONCHO_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        "env"
    } else {
        "missing"
    };
    let base_url_source = if honcho_string(&file_config, &host_config, "baseUrl")
        .or_else(|| honcho_string(&file_config, &host_config, "base_url"))
        .is_some()
    {
        "file"
    } else if env::var_os("HONCHO_BASE_URL")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        "env"
    } else {
        "missing"
    };
    let mut out = serde_json::Map::new();
    out.insert("schema".into(), json!("hermes_honcho_config_desktop_v1"));
    out.insert(
        "configPath".into(),
        json!(config_path.to_string_lossy().to_string()),
    );
    out.insert("configPathExists".into(), json!(config_path.is_file()));
    out.insert(
        "globalConfigPath".into(),
        json!(honcho_global_config_path().to_string_lossy().to_string()),
    );
    out.insert("activeHost".into(), json!(host));
    out.insert("hostBlockPresent".into(), json!(host_config.is_some()));
    out.insert("explicitlyConfigured".into(), json!(explicitly_configured));
    out.insert("configured".into(), json!(configured));
    out.insert("enabled".into(), json!(enabled));
    out.insert("apiKeyConfigured".into(), json!(api_key.is_some()));
    out.insert("apiKeySource".into(), json!(api_key_source));
    out.insert("baseUrl".into(), json!(base_url));
    out.insert("baseUrlSource".into(), json!(base_url_source));
    out.insert(
        "environment".into(),
        json!(honcho_string(&file_config, &host_config, "environment")
            .or(environment_env)
            .unwrap_or_else(|| "production".into())),
    );
    out.insert("timeout".into(), json!(timeout));
    out.insert(
        "workspace".into(),
        json!(
            honcho_string(&file_config, &host_config, "workspace").unwrap_or_else(|| host.clone())
        ),
    );
    out.insert(
        "peerName".into(),
        json!(honcho_string(&file_config, &host_config, "peerName")),
    );
    out.insert(
        "aiPeer".into(),
        json!(honcho_string(&file_config, &host_config, "aiPeer").unwrap_or_else(|| host.clone())),
    );
    out.insert(
        "pinUserPeer".into(),
        json!(
            honcho_bool_alias(&file_config, &host_config, "pinUserPeer", "pinPeerName")
                .unwrap_or(false)
        ),
    );
    out.insert(
        "userPeerAliases".into(),
        honcho_string_map(&file_config, &host_config, "userPeerAliases"),
    );
    out.insert(
        "runtimePeerPrefix".into(),
        json!(honcho_string(&file_config, &host_config, "runtimePeerPrefix").unwrap_or_default()),
    );
    out.insert(
        "saveMessages".into(),
        json!(honcho_bool(&file_config, &host_config, "saveMessages").unwrap_or(true)),
    );
    out.insert(
        "writeFrequency".into(),
        honcho_raw_or_default(&file_config, &host_config, "writeFrequency", json!("async")),
    );
    out.insert(
        "contextTokens".into(),
        json!(honcho_i64(&file_config, &host_config, "contextTokens")),
    );
    out.insert(
        "recallMode".into(),
        json!(normalize_honcho_recall_mode(
            &honcho_string(&file_config, &host_config, "recallMode")
                .unwrap_or_else(|| "hybrid".into())
        )),
    );
    out.insert(
        "initOnSessionStart".into(),
        json!(honcho_bool(&file_config, &host_config, "initOnSessionStart").unwrap_or(false)),
    );
    out.insert(
        "sessionStrategy".into(),
        json!(honcho_string(&file_config, &host_config, "sessionStrategy")
            .unwrap_or_else(|| "per-directory".into())),
    );
    out.insert(
        "sessionPeerPrefix".into(),
        json!(honcho_bool(&file_config, &host_config, "sessionPeerPrefix").unwrap_or(false)),
    );
    out.insert(
        "sessions".into(),
        honcho_raw_or_default(&file_config, &host_config, "sessions", json!({})),
    );
    out.insert("observationMode".into(), json!(observation_mode));
    out.insert("observation".into(), observation);
    out.insert("dialecticReasoningLevel".into(), json!(reasoning_level));
    out.insert(
        "dialecticDynamic".into(),
        json!(honcho_bool(&file_config, &host_config, "dialecticDynamic").unwrap_or(true)),
    );
    out.insert(
        "dialecticMaxChars".into(),
        json!(honcho_i64(&file_config, &host_config, "dialecticMaxChars").unwrap_or(600)),
    );
    out.insert(
        "dialecticMaxInputChars".into(),
        json!(honcho_i64(&file_config, &host_config, "dialecticMaxInputChars").unwrap_or(10000)),
    );
    out.insert("dialecticDepth".into(), json!(dialectic_depth));
    out.insert("dialecticDepthLevels".into(), dialectic_depth_levels);
    out.insert(
        "reasoningHeuristic".into(),
        json!(honcho_bool(&file_config, &host_config, "reasoningHeuristic").unwrap_or(true)),
    );
    out.insert("reasoningLevelCap".into(), json!(reasoning_level_cap));
    out.insert(
        "messageMaxChars".into(),
        json!(honcho_i64(&file_config, &host_config, "messageMaxChars").unwrap_or(25000)),
    );
    out.insert(
        "contextCadence".into(),
        json!(honcho_i64(&file_config, &host_config, "contextCadence").unwrap_or(1)),
    );
    out.insert(
        "dialecticCadence".into(),
        json!(honcho_i64(&file_config, &host_config, "dialecticCadence").unwrap_or(1)),
    );
    out.insert(
        "injectionFrequency".into(),
        json!(
            honcho_string(&file_config, &host_config, "injectionFrequency")
                .unwrap_or_else(|| "every-turn".into())
        ),
    );
    out.insert("hostOverridesRoot".into(), json!(true));
    out.insert(
        "envFallback".into(),
        json!([
            "HONCHO_API_KEY",
            "HONCHO_BASE_URL",
            "HONCHO_ENVIRONMENT",
            "HONCHO_TIMEOUT"
        ]),
    );
    out.insert("hostEnv".into(), json!("HERMES_HONCHO_HOST"));
    out.insert(
        "defaults".into(),
        json!({
            "environment": "production",
            "recallMode": "hybrid",
            "writeFrequency": "async",
            "sessionStrategy": "per-directory",
            "dialecticReasoningLevel": "low",
            "dialecticDepth": 1,
            "dialecticDynamic": true,
            "dialecticMaxChars": 600,
            "dialecticMaxInputChars": 10000,
            "messageMaxChars": 25000,
            "contextCadence": 1,
            "dialecticCadence": 1,
            "injectionFrequency": "every-turn"
        }),
    );
    Value::Object(out)
}

fn supermemory_provider_status() -> Value {
    let config = supermemory_config_snapshot();
    let configured = config["apiKeyConfigured"].as_bool().unwrap_or(false);
    json!({
        "name": "supermemory",
        "description": "Supermemory semantic long-term memory with profile recall, semantic search, explicit memory tools, multi-container support, and session ingest.",
        "available": configured,
        "configured": configured,
        "requiredEnv": ["SUPERMEMORY_API_KEY"],
        "optionalEnv": ["SUPERMEMORY_CONTAINER_TAG"],
        "tools": ["supermemory_store", "supermemory_search", "supermemory_forget", "supermemory_profile"],
        "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_session_end", "shutdown"],
        "source": "hermes_supermemory_provider_desktop_v1",
        "config": config
    })
}

fn supermemory_config_snapshot() -> Value {
    let config_path = hermes_home_dir().join("supermemory.json");
    let file_config = read_json_object(&config_path);
    let api_key_configured = env::var_os("SUPERMEMORY_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    let base_url = env::var("SUPERMEMORY_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.supermemory.ai".into());
    let env_tag = env::var("SUPERMEMORY_CONTAINER_TAG")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let raw_container_tag = env_tag
        .clone()
        .or_else(|| json_object_string(&file_config, "container_tag"))
        .unwrap_or_else(|| "hermes".into());
    let resolved_container_tag =
        sanitize_supermemory_tag(&raw_container_tag.replace("{identity}", "default"));
    let max_recall_results = json_object_i64(&file_config, "max_recall_results")
        .unwrap_or(10)
        .clamp(1, 20);
    let profile_frequency = json_object_i64(&file_config, "profile_frequency")
        .unwrap_or(50)
        .clamp(1, 500);
    let capture_mode =
        if json_object_string(&file_config, "capture_mode").as_deref() == Some("everything") {
            "everything"
        } else {
            "all"
        };
    let search_mode = json_object_string(&file_config, "search_mode")
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "hybrid" | "memories" | "documents"))
        .unwrap_or_else(|| "hybrid".into());
    let entity_context = json_object_string(&file_config, "entity_context")
        .map(|value| clamp_supermemory_entity_context(&value))
        .unwrap_or_else(default_supermemory_entity_context);
    let api_timeout = json_object_f64(&file_config, "api_timeout")
        .unwrap_or(5.0)
        .clamp(0.5, 15.0);
    let custom_containers = file_config
        .as_ref()
        .and_then(|value| value.get("custom_containers"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(sanitize_supermemory_tag)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let allowed_containers = std::iter::once(resolved_container_tag.clone())
        .chain(custom_containers.clone())
        .collect::<Vec<_>>();
    json!({
        "schema": "hermes_supermemory_config_desktop_v1",
        "configPath": config_path.to_string_lossy().to_string(),
        "configPathExists": config_path.is_file(),
        "apiKeyConfigured": api_key_configured,
        "apiKeySource": if api_key_configured { "env" } else { "missing" },
        "baseUrl": base_url,
        "baseUrlSource": if env::var_os("SUPERMEMORY_BASE_URL").is_some() { "env" } else { "default" },
        "containerTag": raw_container_tag,
        "containerTagSource": if env_tag.is_some() { "env" } else if file_config.as_ref().and_then(|value| value.get("container_tag")).is_some() { "file" } else { "default" },
        "resolvedContainerTag": resolved_container_tag,
        "autoRecall": json_object_bool(&file_config, "auto_recall").unwrap_or(true),
        "autoCapture": json_object_bool(&file_config, "auto_capture").unwrap_or(true),
        "maxRecallResults": max_recall_results,
        "profileFrequency": profile_frequency,
        "captureMode": capture_mode,
        "searchMode": search_mode,
        "entityContext": entity_context,
        "entityContextLength": entity_context.len(),
        "apiTimeout": api_timeout,
        "enableCustomContainerTags": json_object_bool(&file_config, "enable_custom_container_tags").unwrap_or(false),
        "customContainers": custom_containers,
        "allowedContainers": allowed_containers,
        "customContainerInstructions": json_object_string(&file_config, "custom_container_instructions").unwrap_or_default().trim().to_string(),
        "fileOverridesEnv": false,
        "envVars": ["SUPERMEMORY_API_KEY", "SUPERMEMORY_CONTAINER_TAG", "SUPERMEMORY_BASE_URL"],
        "defaults": {
            "containerTag": "hermes",
            "autoRecall": true,
            "autoCapture": true,
            "maxRecallResults": 10,
            "profileFrequency": 50,
            "captureMode": "all",
            "searchMode": "hybrid",
            "apiTimeout": 5.0,
            "enableCustomContainerTags": false,
            "customContainers": []
        }
    })
}

fn mem0_config_snapshot() -> Value {
    let config_path = hermes_home_dir().join("mem0.json");
    let file_config = read_json_object(&config_path);
    let get_file_string = |key: &str| {
        file_config
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let get_file_bool = |key: &str| {
        file_config
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(json_value_as_bool)
    };
    let api_key_env = env::var("MEM0_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let user_id_env = env::var("MEM0_USER_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let agent_id_env = env::var("MEM0_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base_url = env::var("MEM0_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.mem0.ai".into());
    let api_key_source = if get_file_string("api_key").is_some() {
        "file"
    } else if api_key_env.is_some() {
        "env"
    } else {
        "missing"
    };
    let user_id = get_file_string("user_id")
        .or(user_id_env)
        .unwrap_or_else(|| "hermes-user".into());
    let agent_id = get_file_string("agent_id")
        .or(agent_id_env)
        .unwrap_or_else(|| "hermes".into());
    let rerank = get_file_bool("rerank").unwrap_or(true);
    let keyword_search = get_file_bool("keyword_search").unwrap_or(false);
    json!({
        "schema": "hermes_mem0_config_desktop_v1",
        "configPath": config_path.to_string_lossy().to_string(),
        "configPathExists": config_path.is_file(),
        "apiKeyConfigured": api_key_source != "missing",
        "apiKeySource": api_key_source,
        "baseUrl": base_url,
        "baseUrlSource": if env::var_os("MEM0_BASE_URL").is_some() { "env" } else { "default" },
        "userId": user_id,
        "agentId": agent_id,
        "rerank": rerank,
        "keywordSearch": keyword_search,
        "fileOverridesEnv": true,
        "envVars": ["MEM0_API_KEY", "MEM0_USER_ID", "MEM0_AGENT_ID", "MEM0_BASE_URL"],
        "defaults": {
            "userId": "hermes-user",
            "agentId": "hermes",
            "rerank": true,
            "keywordSearch": false
        }
    })
}

fn read_json_object(path: &PathBuf) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    value.as_object()?;
    Some(value)
}

fn json_object_string(object: &Option<Value>, key: &str) -> Option<String> {
    object
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
                .or_else(|| value.as_u64().map(|number| number.to_string()))
                .or_else(|| value.as_f64().map(|number| number.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn json_object_i64(object: &Option<Value>, key: &str) -> Option<i64> {
    object
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| {
            value.as_i64().or_else(|| {
                value
                    .as_str()
                    .and_then(|raw| raw.trim().parse::<i64>().ok())
            })
        })
}

fn json_object_f64(object: &Option<Value>, key: &str) -> Option<f64> {
    object
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|raw| raw.trim().parse::<f64>().ok())
            })
        })
}

fn json_object_bool(object: &Option<Value>, key: &str) -> Option<bool> {
    object
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(json_value_as_bool)
}

fn honcho_global_config_path() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".honcho")
        .join("config.json")
}

fn hindsight_legacy_config_path() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hindsight")
        .join("config.json")
}

fn hindsight_embedded_profile_env_path(config: &Option<Value>) -> PathBuf {
    let profile = hindsight_string(config, "profile").unwrap_or_else(|| "hermes".into());
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hindsight")
        .join("profiles")
        .join(format!("{profile}.env"))
}

fn hindsight_raw<'a>(object: &'a Option<Value>, key: &str) -> Option<&'a Value> {
    object.as_ref().and_then(|value| value.get(key))
}

fn hindsight_string(object: &Option<Value>, key: &str) -> Option<String> {
    hindsight_raw(object, key)
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
                .or_else(|| value.as_u64().map(|number| number.to_string()))
                .or_else(|| value.as_f64().map(|number| number.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn hindsight_i64(object: &Option<Value>, key: &str) -> Option<i64> {
    hindsight_raw(object, key).and_then(|value| {
        value.as_i64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().parse::<i64>().ok())
        })
    })
}

fn hindsight_bool(object: &Option<Value>, key: &str) -> Option<bool> {
    hindsight_raw(object, key).and_then(json_value_as_bool)
}

fn normalize_hindsight_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "local" | "local_embedded" => "local_embedded".into(),
        "local_external" => "local_external".into(),
        "cloud" => "cloud".into(),
        _ => "cloud".into(),
    }
}

fn normalize_hindsight_budget(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => "low".into(),
        "high" => "high".into(),
        _ => "mid".into(),
    }
}

fn normalize_hindsight_memory_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "context" => "context".into(),
        "tools" => "tools".into(),
        _ => "hybrid".into(),
    }
}

fn hindsight_tags(value: Option<Value>) -> Vec<String> {
    let raw_items = match value {
        Some(Value::Array(items)) => items,
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Vec::new()
            } else if trimmed.starts_with('[') {
                serde_json::from_str::<Value>(trimmed)
                    .ok()
                    .and_then(|value| value.as_array().cloned())
                    .unwrap_or_else(|| {
                        trimmed
                            .split(',')
                            .map(|item| Value::String(item.to_string()))
                            .collect()
                    })
            } else {
                trimmed
                    .split(',')
                    .map(|item| Value::String(item.to_string()))
                    .collect()
            }
        }
        Some(value) => vec![value],
        None => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    raw_items
        .iter()
        .filter_map(|item| item.as_str().or_else(|| item.as_i64().map(|_| "")))
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert((*item).to_string()))
        .map(ToString::to_string)
        .collect()
}

fn sanitize_hindsight_bank_segment(raw: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            out.push('-');
            previous_dash = true;
        }
    }
    out.trim_matches(['-', '_']).to_string()
}

fn resolve_hindsight_bank_template(
    template: &str,
    fallback: &str,
    placeholders: &[(&str, &str)],
) -> String {
    if template.trim().is_empty() {
        return fallback.to_string();
    }
    let mut rendered = template.to_string();
    for (key, value) in placeholders {
        rendered = rendered.replace(
            &format!("{{{key}}}"),
            &sanitize_hindsight_bank_segment(value),
        );
    }
    while rendered.contains("--") {
        rendered = rendered.replace("--", "-");
    }
    while rendered.contains("__") {
        rendered = rendered.replace("__", "_");
    }
    let rendered = rendered.trim_matches(['-', '_']).to_string();
    if rendered.is_empty() {
        fallback.to_string()
    } else {
        rendered
    }
}

fn honcho_host_block(file_config: &Option<Value>, host: &str) -> Option<Value> {
    let hosts = file_config.as_ref()?.get("hosts")?.as_object()?;
    if let Some(block) = hosts.get(host).filter(|value| value.is_object()) {
        return Some(block.clone());
    }
    if let Some(profile) = host.strip_prefix("hermes_") {
        let legacy = format!("hermes.{profile}");
        return hosts
            .get(&legacy)
            .filter(|value| value.is_object())
            .cloned();
    }
    None
}

fn honcho_raw<'a>(
    root: &'a Option<Value>,
    host: &'a Option<Value>,
    key: &str,
) -> Option<&'a Value> {
    host.as_ref()
        .and_then(|value| value.get(key))
        .or_else(|| root.as_ref().and_then(|value| value.get(key)))
}

fn honcho_raw_or_default(
    root: &Option<Value>,
    host: &Option<Value>,
    key: &str,
    default: Value,
) -> Value {
    honcho_raw(root, host, key).cloned().unwrap_or(default)
}

fn honcho_string(root: &Option<Value>, host: &Option<Value>, key: &str) -> Option<String> {
    honcho_raw(root, host, key)
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
                .or_else(|| value.as_u64().map(|number| number.to_string()))
                .or_else(|| value.as_f64().map(|number| number.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn honcho_i64(root: &Option<Value>, host: &Option<Value>, key: &str) -> Option<i64> {
    honcho_raw(root, host, key).and_then(|value| {
        value.as_i64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().parse::<i64>().ok())
        })
    })
}

fn honcho_f64(root: &Option<Value>, host: &Option<Value>, key: &str) -> Option<f64> {
    honcho_raw(root, host, key).and_then(|value| {
        value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().parse::<f64>().ok())
        })
    })
}

fn honcho_bool(root: &Option<Value>, host: &Option<Value>, key: &str) -> Option<bool> {
    honcho_raw(root, host, key).and_then(json_value_as_bool)
}

fn honcho_bool_alias(
    root: &Option<Value>,
    host: &Option<Value>,
    primary: &str,
    fallback: &str,
) -> Option<bool> {
    host.as_ref()
        .and_then(|value| value.get(primary))
        .and_then(json_value_as_bool)
        .or_else(|| {
            host.as_ref()
                .and_then(|value| value.get(fallback))
                .and_then(json_value_as_bool)
        })
        .or_else(|| {
            root.as_ref()
                .and_then(|value| value.get(primary))
                .and_then(json_value_as_bool)
        })
        .or_else(|| {
            root.as_ref()
                .and_then(|value| value.get(fallback))
                .and_then(json_value_as_bool)
        })
}

fn honcho_string_map(root: &Option<Value>, host: &Option<Value>, key: &str) -> Value {
    let Some(map) = honcho_raw(root, host, key).and_then(Value::as_object) else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (key, value) in map {
        let alias_key = key.trim();
        let alias_value = value.as_str().unwrap_or("").trim();
        if !alias_key.is_empty() && !alias_value.is_empty() {
            out.insert(alias_key.into(), json!(alias_value));
        }
    }
    Value::Object(out)
}

fn normalize_honcho_recall_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" | "hybrid" => "hybrid".into(),
        "context" => "context".into(),
        "tools" => "tools".into(),
        _ => "hybrid".into(),
    }
}

fn normalize_honcho_observation_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "shared" | "unified" => "unified".into(),
        "separate" | "cross" | "directional" => "directional".into(),
        _ => "directional".into(),
    }
}

fn normalize_honcho_reasoning_level(raw: &str, default: &str, allow_max: bool) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "minimal" => "minimal".into(),
        "low" => "low".into(),
        "medium" => "medium".into(),
        "high" => "high".into(),
        "max" if allow_max => "max".into(),
        _ => default.into(),
    }
}

fn honcho_dialectic_depth_levels(
    root: &Option<Value>,
    host: &Option<Value>,
    depth: usize,
) -> Value {
    let Some(items) = honcho_raw(root, host, "dialecticDepthLevels").and_then(Value::as_array)
    else {
        return Value::Null;
    };
    let mut levels = items
        .iter()
        .take(depth)
        .map(|value| normalize_honcho_reasoning_level(value.as_str().unwrap_or("low"), "low", true))
        .collect::<Vec<_>>();
    while levels.len() < depth {
        levels.push("low".into());
    }
    json!(levels)
}

fn honcho_observation_snapshot(root: &Option<Value>, host: &Option<Value>, mode: &str) -> Value {
    let (mut user_observe_me, mut user_observe_others, mut ai_observe_me, mut ai_observe_others) =
        if mode == "unified" {
            (true, false, false, true)
        } else {
            (true, true, true, true)
        };
    if let Some(observation) = honcho_raw(root, host, "observation").and_then(Value::as_object) {
        if let Some(user) = observation.get("user").and_then(Value::as_object) {
            user_observe_me = user
                .get("observeMe")
                .and_then(json_value_as_bool)
                .unwrap_or(user_observe_me);
            user_observe_others = user
                .get("observeOthers")
                .and_then(json_value_as_bool)
                .unwrap_or(user_observe_others);
        }
        if let Some(ai) = observation.get("ai").and_then(Value::as_object) {
            ai_observe_me = ai
                .get("observeMe")
                .and_then(json_value_as_bool)
                .unwrap_or(ai_observe_me);
            ai_observe_others = ai
                .get("observeOthers")
                .and_then(json_value_as_bool)
                .unwrap_or(ai_observe_others);
        }
    }
    json!({
        "userObserveMe": user_observe_me,
        "userObserveOthers": user_observe_others,
        "aiObserveMe": ai_observe_me,
        "aiObserveOthers": ai_observe_others
    })
}

fn json_value_as_bool(value: &Value) -> Option<bool> {
    if let Some(boolean) = value.as_bool() {
        return Some(boolean);
    }
    value
        .as_str()
        .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" | "on" => Some(true),
            "false" | "0" | "no" | "n" | "off" => Some(false),
            _ => None,
        })
}

fn sanitize_supermemory_tag(raw: &str) -> String {
    let mut out = String::new();
    let mut last_was_underscore = false;
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' {
            ch
        } else {
            '_'
        };
        if mapped == '_' {
            if !last_was_underscore {
                out.push(mapped);
            }
            last_was_underscore = true;
        } else {
            out.push(mapped);
            last_was_underscore = false;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "hermes".into()
    } else {
        trimmed
    }
}

fn clamp_supermemory_entity_context(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return default_supermemory_entity_context();
    }
    text.chars().take(1500).collect()
}

fn default_supermemory_entity_context() -> String {
    "User-assistant conversation. Format: [role: user]...[user:end] and [role: assistant]...[assistant:end].\n\nOnly extract things useful in future conversations. Most messages are not worth remembering.\n\nRemember lasting personal facts, preferences, routines, tools, ongoing projects, working context, and explicit requests to remember something.\n\nDo not remember temporary intents, one-time tasks, assistant actions, implementation details, or in-progress status.\n\nWhen in doubt, store less.".into()
}

fn byterover_provider_status() -> Value {
    let cwd = byterover_working_dir();
    let cli_path = byterover_cli_candidates()
        .into_iter()
        .find(|path| path.is_file());
    let api_key_configured = env::var_os("BRV_API_KEY")
        .filter(|value| !value.is_empty())
        .is_some();
    let configured = cli_path.is_some() || cwd.is_dir() || api_key_configured;
    json!({
        "name": "byterover",
        "description": "ByteRover persistent knowledge tree with tiered retrieval via the brv CLI.",
        "available": cli_path.is_some(),
        "configured": configured,
        "requiredEnv": ["BRV_API_KEY"],
        "requiredRuntime": ["brv CLI"],
        "tools": ["byterover_status", "brv_query", "brv_curate", "brv_status"],
        "hermesTools": ["brv_query", "brv_curate", "brv_status"],
        "hooks": ["prefetch", "sync_turn", "on_memory_write", "on_pre_compress"],
        "workingDirectory": cwd.to_string_lossy().to_string(),
        "source": "hermes_byterover_status_desktop_v1"
    })
}

fn provider_status(
    name: &str,
    description: &str,
    required_env: &[&str],
    tools: &[&str],
    hooks: &[&str],
) -> Value {
    let configured = required_env.iter().any(|key| std::env::var(key).is_ok());
    json!({
        "name": name,
        "description": description,
        "available": configured,
        "configured": configured,
        "requiredEnv": required_env,
        "tools": tools,
        "hooks": hooks,
        "source": "hermes_external_provider_boundary"
    })
}

fn memory_provider_boundary(store: &AppStore) -> Value {
    json!({
        "kind": "hermes_memory_provider_desktop_v1",
        "activeProviderEnv": ["SYNTHCHAT_MEMORY_PROVIDER", "HERMES_MEMORY_PROVIDER", "MEMORY_PROVIDER"],
        "oneActiveProvider": true,
        "bundledProviders": ["builtin", "holographic", "supermemory", "honcho", "mem0", "openviking", "byterover", "hindsight", "retaindb"],
        "localStateDir": memory_provider_state_dir(store),
        "externalBoundary": "Cloud/SDK providers are surfaced with Hermes tool names and readiness diagnostics; end-to-end execution requires the configured provider runtime."
    })
}

fn memory_provider_state_dir(store: &AppStore) -> String {
    store
        .data_dir()
        .join("memory-providers")
        .to_string_lossy()
        .to_string()
}

fn hermes_memory_provider_tool_names() -> Vec<&'static str> {
    vec![
        "memory_provider",
        "fact_store",
        "fact_feedback",
        "supermemory_store",
        "supermemory_search",
        "supermemory_forget",
        "supermemory_profile",
        "honcho_profile",
        "honcho_search",
        "honcho_reasoning",
        "honcho_context",
        "honcho_conclude",
        "mem0_profile",
        "mem0_search",
        "mem0_conclude",
        "viking_search",
        "viking_read",
        "viking_browse",
        "viking_remember",
        "viking_add_resource",
        "byterover_status",
        "hindsight_reflect",
        "hindsight_search",
        "hindsight_remember",
        "retaindb_profile",
        "retaindb_search",
        "retaindb_store",
        "retaindb_context",
        "retaindb_remember",
        "retaindb_forget",
        "retaindb_upload_file",
        "retaindb_list_files",
        "retaindb_read_file",
        "retaindb_ingest_file",
        "retaindb_delete_file",
        "retaindb_ingest_session",
        "retaindb_agent_model",
        "retaindb_seed_agent",
    ]
}

fn provider_for_memory_tool(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "supermemory_store"
        | "supermemory_search"
        | "supermemory_forget"
        | "supermemory_profile" => Some("supermemory"),
        "honcho_profile" | "honcho_search" | "honcho_reasoning" | "honcho_context"
        | "honcho_conclude" => Some("honcho"),
        "mem0_profile" | "mem0_search" | "mem0_conclude" => Some("mem0"),
        "viking_search"
        | "viking_read"
        | "viking_browse"
        | "viking_remember"
        | "viking_add_resource" => Some("openviking"),
        "byterover_status" | "brv_query" | "brv_curate" | "brv_status" => Some("byterover"),
        "hindsight_reflect" | "hindsight_search" | "hindsight_remember" => Some("hindsight"),
        "retaindb_search"
        | "retaindb_store"
        | "retaindb_profile"
        | "retaindb_context"
        | "retaindb_remember"
        | "retaindb_forget"
        | "retaindb_upload_file"
        | "retaindb_list_files"
        | "retaindb_read_file"
        | "retaindb_ingest_file"
        | "retaindb_delete_file"
        | "retaindb_ingest_session"
        | "retaindb_agent_model"
        | "retaindb_seed_agent" => Some("retaindb"),
        _ => None,
    }
}

fn required_env_for_provider(provider: &str) -> Vec<&'static str> {
    match provider {
        "supermemory" => vec!["SUPERMEMORY_API_KEY"],
        "honcho" => vec!["HONCHO_API_KEY", "HONCHO_BASE_URL"],
        "mem0" => vec!["MEM0_API_KEY"],
        "openviking" => vec!["OPENVIKING_ENDPOINT", "OPENVIKING_API_KEY"],
        "byterover" => vec!["BRV_HOME"],
        "hindsight" => vec!["HINDSIGHT_API_KEY", "HINDSIGHT_PROJECT"],
        "retaindb" => vec!["RETAINDB_API_KEY"],
        _ => vec![],
    }
}

fn holographic_state_dir(store: &AppStore) -> PathBuf {
    store
        .data_dir()
        .join("memory-providers")
        .join("holographic")
}

fn holographic_facts_path(store: &AppStore) -> String {
    holographic_state_dir(store)
        .join("facts.json")
        .to_string_lossy()
        .to_string()
}

fn holographic_boundary(store: &AppStore) -> Value {
    json!({
        "kind": "hermes_holographic_memory_desktop_v1",
        "statePath": holographic_facts_path(store),
        "features": ["add", "search", "probe", "related", "reason", "contradict", "update", "remove", "list", "feedback"],
        "storage": "json_desktop_adaptation",
        "hrrBoundary": "Uses lexical/entity scoring without Python numpy HRR vectors; preserves Hermes fact-store tool semantics for desktop native execution."
    })
}

fn load_holographic_facts(store: &AppStore) -> AppResult<Vec<Value>> {
    let path = PathBuf::from(holographic_facts_path(store));
    if !path.exists() {
        return Ok(vec![]);
    }
    let text = fs::read_to_string(&path)?;
    let value: Value = serde_json::from_str(&text)?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

fn save_holographic_facts(store: &AppStore, facts: &[Value]) -> AppResult<()> {
    let dir = holographic_state_dir(store);
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("facts.json"),
        serde_json::to_string_pretty(&Value::Array(facts.to_vec()))?,
    )?;
    Ok(())
}

fn normalize_fact_category(category: &str) -> String {
    match category.trim().to_ascii_lowercase().as_str() {
        "user_pref" | "preference" | "preferences" => "user_pref".into(),
        "project" | "tool" | "general" => category.trim().to_ascii_lowercase(),
        _ => "general".into(),
    }
}

fn parse_fact_tags(payload: &Value) -> Vec<String> {
    if let Some(items) = payload.get("tags").and_then(Value::as_array) {
        return items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect();
    }
    string_arg(payload, &["tags"])
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn extract_fact_entities(content: &str) -> Vec<String> {
    let mut entities = BTreeSet::new();
    for token in content
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '.'))
        .map(str::trim)
        .filter(|token| token.len() >= 2)
    {
        let starts_named = token
            .chars()
            .next()
            .map(|ch| ch.is_uppercase() || ch.is_ascii_digit())
            .unwrap_or(false);
        if starts_named || token.contains('.') || token.contains('-') {
            entities.insert(token.trim_matches('.').to_string());
        }
    }
    entities.into_iter().take(24).collect()
}

fn memory_content_looks_like_preference(content: &str) -> bool {
    let text = content.to_ascii_lowercase();
    [
        "prefer", "prefers", "like", "likes", "hate", "hates", "favorite", "default",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn fact_matches_entity(fact: &Value, entity_lc: &str) -> bool {
    fact["content"]
        .as_str()
        .map(|content| content.to_ascii_lowercase().contains(entity_lc))
        .unwrap_or(false)
        || fact["entities"]
            .as_array()
            .map(|entities| {
                entities.iter().any(|entity| {
                    entity
                        .as_str()
                        .map(|text| text.to_ascii_lowercase() == entity_lc)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
}

fn rank_holographic_facts(facts: &[Value], query: &str, payload: &Value) -> Vec<Value> {
    let category = string_arg(payload, &["category"]).map(|value| normalize_fact_category(&value));
    let min_trust = payload
        .get("min_trust")
        .or_else(|| payload.get("minTrust"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let query_lc = query.trim().to_ascii_lowercase();
    let mut ranked = facts
        .iter()
        .filter(|fact| {
            category
                .as_ref()
                .map(|expected| fact["category"].as_str() == Some(expected.as_str()))
                .unwrap_or(true)
        })
        .filter(|fact| fact["trust"].as_f64().unwrap_or(0.5) >= min_trust)
        .filter_map(|fact| {
            let content = fact["content"].as_str().unwrap_or("").to_ascii_lowercase();
            let score = if query_lc.is_empty() {
                (fact["trust"].as_f64().unwrap_or(0.5) * 100.0) as u32
            } else if content.contains(&query_lc) {
                1000 + query_lc.len() as u32
            } else {
                query_lc
                    .split_whitespace()
                    .filter(|term| !term.is_empty() && content.contains(*term))
                    .map(|term| 20 + term.len() as u32)
                    .sum::<u32>()
            };
            (score > 0).then(|| {
                let mut item = fact.clone();
                item["score"] = json!(score);
                (score, item)
            })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| {
                right["trust"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&left["trust"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right["updatedAt"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(left["updatedAt"].as_str().unwrap_or(""))
            })
    });
    ranked.into_iter().map(|(_, fact)| fact).collect()
}

fn holographic_contradictions(facts: &[Value]) -> Vec<Value> {
    let mut results = Vec::new();
    for (idx, left) in facts.iter().enumerate() {
        let left_content = left["content"].as_str().unwrap_or("").to_ascii_lowercase();
        for right in facts.iter().skip(idx + 1) {
            let right_content = right["content"].as_str().unwrap_or("").to_ascii_lowercase();
            let shared_entity = left["entities"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|entity| {
                    entity
                        .as_str()
                        .map(|entity| fact_matches_entity(right, &entity.to_ascii_lowercase()))
                        .unwrap_or(false)
                });
            if shared_entity
                && ((left_content.contains(" never ") && right_content.contains(" always "))
                    || (left_content.contains(" always ") && right_content.contains(" never "))
                    || (left_content.contains(" prefer ") && right_content.contains(" hate "))
                    || (left_content.contains(" hate ") && right_content.contains(" prefer ")))
            {
                results.push(json!({"left": left, "right": right, "reason": "shared entity with opposing lexical cue"}));
            }
        }
    }
    results
}
