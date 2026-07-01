use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{ChatMessage, Conversation, MemoryEntry},
    store::AppStore,
};

use super::{string_arg, truncate_for_prompt};

pub(super) fn session_search_tool(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let current_conversation = store.conversation(current_conversation_id)?;
    let (text, raw) = execute_session_search(store, &current_conversation, payload)?;
    Ok(format!(
        "{text}\n\njson:\n{}",
        serde_json::to_string_pretty(&raw)?
    ))
}

pub(super) fn execute_session_search(
    store: &AppStore,
    conversation: &Conversation,
    payload: &Value,
) -> AppResult<(String, Value)> {
    let anchor_conversation_id = session_search_payload_string(
        payload,
        &[
            "conversationId",
            "conversation_id",
            "sessionId",
            "session_id",
        ],
    );
    let anchor_message_id = session_search_payload_string(
        payload,
        &[
            "messageId",
            "message_id",
            "aroundMessageId",
            "around_message_id",
        ],
    );
    if let (Some(conversation_id), Some(message_id)) = (anchor_conversation_id, anchor_message_id) {
        return execute_session_scroll(store, &conversation_id, &message_id, payload);
    }

    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20) as usize;
    let offset = payload
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(200) as usize;
    let kind = payload
        .get("kind")
        .or_else(|| payload.get("source"))
        .and_then(Value::as_str)
        .map(normalize_session_search_kind)
        .unwrap_or_else(|| "all".into());
    let window = payload
        .get("window")
        .or_else(|| payload.get("contextWindow"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(0, 3) as usize;
    let sort = payload
        .get("sort")
        .and_then(Value::as_str)
        .unwrap_or("relevance")
        .trim()
        .to_lowercase();
    let include_subagents = payload
        .get("includeSubagents")
        .or_else(|| payload.get("include_subagents"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if query.is_empty() {
        if kind == "session_memory" {
            return execute_session_memory_browse(store, conversation, limit, offset);
        }
        return execute_session_browse(store, conversation, limit, offset);
    }

    let conversations = store.conversations()?;
    let conversation_titles = conversations
        .iter()
        .map(|item| (item.id.clone(), item.title.clone()))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::new();
    let mut recency = 0usize;

    if session_search_kind_matches(&kind, "message") {
        for convo in &conversations {
            let messages = store.messages(&convo.id, None)?;
            for (index, message) in messages.iter().enumerate().rev() {
                push_session_search_candidate(
                    &mut candidates,
                    query.as_str(),
                    recency,
                    30,
                    "message".into(),
                    convo.id.clone(),
                    Some(message.id.clone()),
                    format!(
                        "message:{}:{}:{}",
                        convo.title, message.role, message.created_at
                    ),
                    build_message_context_window(&messages, index, window),
                    500,
                    None,
                );
                recency += 1;
            }
        }
    }

    let runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| include_subagents || run.parent_run_id.is_none())
        .collect::<Vec<_>>();
    let run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect::<HashSet<_>>();
    if session_search_kind_matches(&kind, "run") {
        for run in runs.iter().rev() {
            let title = conversation_titles
                .get(&run.conversation_id)
                .cloned()
                .unwrap_or_else(|| run.conversation_id.clone());
            let summary = format!(
                "conversation={} run={} state={} request={} error={}",
                title,
                run.run_id,
                run.state,
                run.user_request,
                run.error.as_deref().unwrap_or("")
            );
            push_session_search_candidate(
                &mut candidates,
                query.as_str(),
                recency,
                25,
                "run".into(),
                run.conversation_id.clone(),
                None,
                format!("run:{}:{}", title, run.run_id),
                summary,
                500,
                None,
            );
            recency += 1;
        }
    }

    if session_search_kind_matches(&kind, "tool") {
        for trace in store.tool_traces()?.into_iter().rev() {
            let Some(run_id) = trace.event.run_id.as_deref() else {
                continue;
            };
            if !run_ids.contains(run_id) {
                continue;
            }
            let conversation_id = runs
                .iter()
                .find(|run| run.run_id == run_id)
                .map(|run| run.conversation_id.clone())
                .unwrap_or_else(|| conversation.id.clone());
            let title = conversation_titles
                .get(&conversation_id)
                .cloned()
                .unwrap_or_else(|| conversation_id.clone());
            let summary = format!(
                "conversation={} {}.{} ok={} summary={} text={} error={}",
                title,
                trace.server_id,
                trace.tool_name,
                trace.ok,
                trace.event.summary,
                trace.event.text.as_deref().unwrap_or(""),
                trace.error.as_deref().unwrap_or("")
            );
            push_session_search_candidate(
                &mut candidates,
                query.as_str(),
                recency,
                28,
                "tool".into(),
                conversation_id,
                None,
                format!("tool:{}:{}:{}", title, trace.server_id, trace.tool_name),
                summary,
                700,
                None,
            );
            recency += 1;
        }
    }

    if session_search_kind_matches(&kind, "artifact") {
        for run in runs.iter().rev() {
            let title = conversation_titles
                .get(&run.conversation_id)
                .cloned()
                .unwrap_or_else(|| run.conversation_id.clone());
            for artifact in store.tool_artifacts_for_run(&run.run_id)? {
                let file_name = artifact
                    .get("fileName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool output");
                let path = artifact
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let preview = artifact
                    .get("contentPreview")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let summary = format!(
                    "conversation={} run={} artifact={} path={} preview={}",
                    title, run.run_id, file_name, path, preview
                );
                push_session_search_candidate(
                    &mut candidates,
                    query.as_str(),
                    recency,
                    27,
                    "artifact".into(),
                    run.conversation_id.clone(),
                    None,
                    format!("artifact:{}:{}:{}", title, run.run_id, file_name),
                    summary,
                    700,
                    Some(artifact),
                );
                recency += 1;
            }
        }
    }

    if session_search_kind_matches(&kind, "session_memory") {
        let session_memories = store
            .memories(None)?
            .into_iter()
            .filter(|memory| memory.target == "session")
            .collect::<Vec<_>>();
        for memory in session_memories {
            push_session_memory_candidate(&mut candidates, query.as_str(), recency, memory);
            recency += 1;
        }
    }

    sort_session_search_candidates(&mut candidates, &sort);
    let total = candidates.len();
    let rows = candidates
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let text = if rows.is_empty() {
        "未找到相关会话记录。".into()
    } else {
        rows.iter()
            .map(|row| {
                let anchor = row
                    .message_id
                    .as_deref()
                    .map(|id| format!(" messageId={id}"))
                    .unwrap_or_default();
                let conversation = if row.conversation_id.is_empty() {
                    " deletedConversation=true".into()
                } else {
                    format!(" conversationId={}", row.conversation_id)
                };
                format!(
                    "- [{} score={}{}{}] {}",
                    row.source, row.score, conversation, anchor, row.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let raw_results = rows
        .iter()
        .map(|row| {
            let conversation_deleted = row.conversation_id.is_empty();
            json!({
                "source": row.source,
                "kind": row.kind,
                "score": row.score,
                "conversationDeleted": conversation_deleted,
                "conversationId": row.conversation_id,
                "session_id": row.conversation_id,
                "messageId": row.message_id,
                "match_message_id": row.message_id,
                "snippet": row.content,
                "content": row.content,
                "metadata": row.metadata
            })
        })
        .collect::<Vec<_>>();
    Ok((
        text,
        json!({
            "success": true,
            "mode": "discover",
            "query": query,
            "kind": kind,
            "limit": limit,
            "offset": offset,
            "total": total,
            "count": raw_results.len(),
            "sessions_searched": total,
            "window": window,
            "sort": sort,
            "includeSubagents": include_subagents,
            "results": raw_results
        }),
    ))
}

#[derive(Debug, Clone)]
pub(super) struct SessionSearchCandidate {
    pub(super) kind: String,
    pub(super) conversation_id: String,
    pub(super) message_id: Option<String>,
    pub(super) source: String,
    pub(super) content: String,
    pub(super) metadata: Option<Value>,
    pub(super) score: i64,
    pub(super) recency: usize,
}

fn push_session_search_candidate(
    candidates: &mut Vec<SessionSearchCandidate>,
    query: &str,
    recency: usize,
    kind_weight: i64,
    kind: String,
    conversation_id: String,
    message_id: Option<String>,
    source: String,
    content: String,
    max_chars: usize,
    metadata: Option<Value>,
) {
    let relevance = session_search_relevance_score(&format!("{source}\n{content}"), query);
    if relevance == 0 {
        return;
    }
    let recency_score = 10_000i64.saturating_sub(recency.min(10_000) as i64);
    candidates.push(SessionSearchCandidate {
        kind,
        conversation_id,
        message_id,
        source,
        content: truncate_for_prompt(&content, max_chars),
        metadata,
        score: relevance as i64 * 1_000 + kind_weight + recency_score,
        recency,
    });
}

fn push_session_memory_candidate(
    candidates: &mut Vec<SessionSearchCandidate>,
    query: &str,
    recency: usize,
    memory: MemoryEntry,
) {
    let content = format!(
        "deleted_session_memory personaId={} importance={} updated={} summary={}",
        memory.persona_id, memory.importance, memory.updated_at, memory.summary
    );
    push_session_search_candidate(
        candidates,
        query,
        recency,
        29,
        "session_memory".into(),
        String::new(),
        None,
        format!("session_memory:{}:{}", memory.id, memory.updated_at),
        content,
        700,
        Some(json!({
            "memoryId": memory.id,
            "target": memory.target,
            "personaId": memory.persona_id,
            "importance": memory.importance,
            "createdAt": memory.created_at,
            "updatedAt": memory.updated_at,
            "summary": memory.summary,
            "source": "deleted_conversation_memory",
            "conversationDeleted": true
        })),
    );
}

fn execute_session_browse(
    store: &AppStore,
    current_conversation: &Conversation,
    limit: usize,
    offset: usize,
) -> AppResult<(String, Value)> {
    let conversations = store.conversations()?;
    let rows = conversations
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let text = if rows.is_empty() {
        "暂无可浏览会话。".into()
    } else {
        rows.iter()
            .map(|item| {
                let current = if item.id == current_conversation.id {
                    " current=true"
                } else {
                    ""
                };
                format!(
                    "- [conversationId={}{}] {} updated={} preview={}",
                    item.id,
                    current,
                    item.title,
                    item.updated_at,
                    truncate_for_prompt(&item.last_message, 160)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let raw_results = rows
        .iter()
        .map(|item| {
            json!({
                "conversationId": item.id,
                "session_id": item.id,
                "title": item.title,
                "updatedAt": item.updated_at,
                "last_active": item.updated_at,
                "createdAt": item.created_at,
                "started_at": item.created_at,
                "lastMessage": item.last_message,
                "preview": item.last_message,
                "current": item.id == current_conversation.id
            })
        })
        .collect::<Vec<_>>();
    Ok((
        text,
        json!({
            "success": true,
            "mode": "browse",
            "limit": limit,
            "offset": offset,
            "count": raw_results.len(),
            "results": raw_results,
            "message": format!("Showing {} most recent sessions. Pass query to search, or session_id + around_message_id to scroll.", raw_results.len())
        }),
    ))
}

fn execute_session_memory_browse(
    store: &AppStore,
    _current_conversation: &Conversation,
    limit: usize,
    offset: usize,
) -> AppResult<(String, Value)> {
    let rows = store
        .memories(None)?
        .into_iter()
        .filter(|memory| memory.target == "session")
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let text = if rows.is_empty() {
        "暂无已整理的删除会话记忆。".into()
    } else {
        rows.iter()
            .map(|memory| {
                format!(
                    "- [session_memory memoryId={} importance={} updated={}] {}",
                    memory.id,
                    memory.importance,
                    memory.updated_at,
                    truncate_for_prompt(&memory.summary, 240)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let raw_results = rows
        .iter()
        .map(|memory| {
            json!({
                "source": "session_memory",
                "kind": "session_memory",
                "memoryId": memory.id,
                "target": memory.target,
                "personaId": memory.persona_id,
                "importance": memory.importance,
                "createdAt": memory.created_at,
                "updatedAt": memory.updated_at,
                "summary": memory.summary,
                "content": memory.summary,
                "conversationDeleted": true,
                "metadata": {
                    "source": "deleted_conversation_memory",
                    "conversationDeleted": true
                }
            })
        })
        .collect::<Vec<_>>();
    Ok((
        text,
        json!({
            "success": true,
            "mode": "browse_session_memory",
            "kind": "session_memory",
            "limit": limit,
            "offset": offset,
            "count": raw_results.len(),
            "results": raw_results,
            "message": format!("Showing {} deleted-session memory summaries. Pass query to search within them.", raw_results.len())
        }),
    ))
}

fn execute_session_scroll(
    store: &AppStore,
    conversation_id: &str,
    message_id: &str,
    payload: &Value,
) -> AppResult<(String, Value)> {
    let window = payload
        .get("window")
        .or_else(|| payload.get("contextWindow"))
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 20) as usize;
    let conversation = store.conversation(conversation_id)?;
    let messages = store.messages(conversation_id, None)?;
    let Some(index) = messages.iter().position(|message| message.id == message_id) else {
        return Ok((
            format!("未在会话 {conversation_id} 中找到消息锚点 {message_id}。"),
            json!({"mode": "scroll", "conversationId": conversation_id, "messageId": message_id, "found": false}),
        ));
    };
    let start = index.saturating_sub(window);
    let end = (index + window + 1).min(messages.len());
    let selected = &messages[start..end];
    let text = selected
        .iter()
        .map(|message| {
            let marker = if message.id == message_id { "*" } else { " " };
            format!(
                "{}{} messageId={} @ {}: {}",
                marker,
                message.role,
                message.id,
                message.created_at,
                truncate_for_prompt(&message.content, 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let raw_messages = selected
        .iter()
        .map(|message| {
            json!({
                "id": message.id,
                "role": message.role,
                "content": message.content,
                "createdAt": message.created_at,
                "timestamp": message.created_at,
                "anchor": message.id == message_id
            })
        })
        .collect::<Vec<_>>();
    Ok((
        format!(
            "会话：{} ({})\nmessagesBefore={} messagesAfter={}\n{}",
            conversation.title,
            conversation.id,
            start,
            messages.len().saturating_sub(end),
            text
        ),
        json!({
            "success": true,
            "mode": "scroll",
            "conversationId": conversation.id,
            "session_id": conversation.id,
            "title": conversation.title,
            "messageId": message_id,
            "around_message_id": message_id,
            "found": true,
            "window": window,
            "messagesBefore": start,
            "messages_before": start,
            "messagesAfter": messages.len().saturating_sub(end),
            "messages_after": messages.len().saturating_sub(end),
            "messages": raw_messages
        }),
    ))
}

fn session_search_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(|value| match value {
            Value::String(raw) => Some(raw.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .or_else(|| string_arg(payload, keys))
}

pub(super) fn sort_session_search_candidates(
    candidates: &mut [SessionSearchCandidate],
    sort: &str,
) {
    match sort {
        "oldest" => candidates.sort_by(|left, right| {
            right
                .recency
                .cmp(&left.recency)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        }),
        "newest" => candidates.sort_by(|left, right| {
            left.recency
                .cmp(&right.recency)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        }),
        _ => candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.recency.cmp(&right.recency))
                .then_with(|| left.source.cmp(&right.source))
        }),
    }
}

pub(super) fn session_search_relevance_score(text: &str, query: &str) -> u32 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 1;
    }
    let text = text.to_lowercase();
    let terms = query
        .split_whitespace()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let terms = if terms.is_empty() {
        vec![query.as_str()]
    } else {
        terms
    };
    let mut score: u32 = if text.contains(&query) { 50 } else { 0 };
    for term in terms {
        let count = text.matches(term).count() as u32;
        score = score.saturating_add(count.saturating_mul(10));
    }
    score
}

fn normalize_session_search_kind(kind: &str) -> String {
    match kind.trim().to_lowercase().replace('-', "_").as_str() {
        "" | "all" | "*" => "all".into(),
        "message" | "messages" | "chat" | "conversation" => "message".into(),
        "run" | "runs" | "agent_run" | "agent_runs" => "run".into(),
        "tool" | "tools" | "tool_trace" | "tool_traces" => "tool".into(),
        "artifact" | "artifacts" | "tool_artifact" | "tool_artifacts" | "file" | "files" => {
            "artifact".into()
        }
        "memory" | "memories" | "session_memory" | "session_memories"
        | "deleted_session_memory" | "deleted_session_memories"
        | "conversation_memory" | "conversation_memories" | "session_summary"
        | "session_summaries" | "short_memory" | "short_memories" | "short_term_memory"
        | "short_term_memories" => "session_memory".into(),
        _ => "all".into(),
    }
}

fn session_search_kind_matches(kind: &str, candidate_kind: &str) -> bool {
    kind == "all" || kind == candidate_kind || (kind == "tool" && candidate_kind == "artifact")
}

fn build_message_context_window(messages: &[ChatMessage], index: usize, window: usize) -> String {
    let start = index.saturating_sub(window);
    let end = (index + window + 1).min(messages.len());
    messages[start..end]
        .iter()
        .enumerate()
        .map(|(offset, message)| {
            let absolute_index = start + offset;
            let marker = if absolute_index == index { "*" } else { " " };
            format!(
                "{}{} {}: {}",
                marker,
                message.role,
                message.created_at,
                truncate_for_prompt(&message.content, 300)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
