use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::json;

use crate::{
    error::AppResult,
    models::{MemoryEntry, Persona},
    store::AppStore,
};

use super::{
    append_parent_phase_event,
    memory::{holographic_memory_prefetch_facts, mirror_memory_write_to_holographic},
};

const MEMORY_OPEN_TAG: &str = "<memory-context>";
const MEMORY_CLOSE_TAG: &str = "</memory-context>";
const HERMES_MEMORY_DIR: &str = ".hermes/memories";
const HERMES_MEMORY_FILE: &str = "MEMORY.md";
const HERMES_USER_FILE: &str = "USER.md";

fn hermes_memory_root(store: &AppStore) -> PathBuf {
    if let Some(home) = std::env::var_os("HERMES_HOME") {
        let root = PathBuf::from(home);
        if root.ends_with(".hermes") {
            return root.join("memories");
        }
        return root.join(HERMES_MEMORY_DIR);
    }
    store.data_dir().join(HERMES_MEMORY_DIR)
}

fn hermes_memory_file_path(store: &AppStore, target: &str) -> PathBuf {
    let name = if target.eq_ignore_ascii_case("user") {
        HERMES_USER_FILE
    } else {
        HERMES_MEMORY_FILE
    };
    hermes_memory_root(store).join(name)
}

fn ensure_hermes_memory_dir(store: &AppStore) -> AppResult<PathBuf> {
    let dir = hermes_memory_root(store);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn render_hermes_memory_markdown(memories: &[MemoryEntry], target: &str) -> String {
    let title = if target.eq_ignore_ascii_case("user") {
        "# USER"
    } else {
        "# MEMORY"
    };
    let mut lines = vec![
        title.to_string(),
        String::new(),
        format!(
            "<!-- SynthChat Hermes-compatible snapshot: target={}, entries={} -->",
            target,
            memories.len()
        ),
        String::new(),
    ];
    for memory in memories {
        let summary = sanitize_memory_context(&memory.summary);
        if summary.trim().is_empty() {
            continue;
        }
        lines.push(format!(
            "- [{}] ({}) {}",
            memory.importance,
            memory.updated_at,
            summary.replace('\n', " ")
        ));
    }
    if lines.len() == 4 {
        lines.push("- (empty)".into());
    }
    lines.push(String::new());
    lines.join("\n")
}

fn parse_hermes_memory_markdown(
    store: &AppStore,
    persona: &Persona,
    target: &str,
    path: &Path,
) -> Vec<MemoryEntry> {
    let Ok(raw) = fs::read_to_string(path) else {
        return vec![];
    };
    let mut items = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- [") {
            continue;
        }
        let Some(after_open) = trimmed.strip_prefix("- [") else {
            continue;
        };
        let Some((importance_text, rest)) = after_open.split_once(']') else {
            continue;
        };
        let importance = importance_text
            .trim()
            .parse::<u8>()
            .unwrap_or(3)
            .clamp(1, 5);
        let mut summary = rest.trim();
        if let Some(after_date) = summary
            .strip_prefix('(')
            .and_then(|value| value.split_once(')'))
        {
            summary = after_date.1.trim();
        }
        if summary.is_empty() || summary == "(empty)" {
            continue;
        }
        if crate::store::scan_memory_content(summary).is_some() {
            continue;
        }
        items.push(MemoryEntry {
            id: String::new(),
            persona_id: persona.id.clone(),
            target: target.to_string(),
            summary: summary.to_string(),
            importance,
            created_at: String::new(),
            updated_at: String::new(),
        });
    }
    items
}

pub(crate) fn sync_builtin_memory_markdown(store: &AppStore, persona: &Persona) -> AppResult<()> {
    ensure_hermes_memory_dir(store)?;
    let all = store.memories(Some(&persona.id))?;
    for target in ["memory", "user"] {
        let items = all
            .iter()
            .filter(|memory| memory.target == target)
            .cloned()
            .collect::<Vec<_>>();
        fs::write(
            hermes_memory_file_path(store, target),
            render_hermes_memory_markdown(&items, target),
        )?;
    }
    Ok(())
}

pub(crate) fn import_builtin_memory_markdown(store: &AppStore, persona: &Persona) -> AppResult<()> {
    let mut by_summary: BTreeMap<(String, String), MemoryEntry> = store
        .memories(Some(&persona.id))?
        .into_iter()
        .map(|memory| ((memory.target.clone(), memory.summary.clone()), memory))
        .collect();
    let mut changed = false;
    for target in ["memory", "user"] {
        let path = hermes_memory_file_path(store, target);
        for parsed in parse_hermes_memory_markdown(store, persona, target, &path) {
            let key = (parsed.target.clone(), parsed.summary.clone());
            if by_summary.contains_key(&key) {
                continue;
            }
            let saved = store.save_memory(parsed)?;
            by_summary.insert(key, saved);
            changed = true;
        }
    }
    if changed {
        sync_builtin_memory_markdown(store, persona)?;
    }
    Ok(())
}

pub(super) fn builtin_memory_prefetch(
    store: &AppStore,
    persona: &Persona,
    query: &str,
) -> AppResult<Vec<MemoryEntry>> {
    let _ = import_builtin_memory_markdown(store, persona);
    let enabled = persona
        .memory
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let include_in_prompt = persona
        .memory
        .get("includeInPrompt")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !enabled || !include_in_prompt {
        return Ok(vec![]);
    }
    let max_memories = persona
        .memory
        .get("maxMemories")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(50)
        .max(1) as usize;
    let mut ranked = store
        .memories(Some(&persona.id))?
        .into_iter()
        .filter(|memory| matches!(memory.target.as_str(), "memory" | "user"))
        .filter(|memory| crate::store::scan_memory_content(&memory.summary).is_none())
        .map(|memory| (memory_prefetch_score(&memory, query), memory))
        .filter(|(score, _)| *score > 0)
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
    memories.truncate(max_memories);
    Ok(memories)
}

pub(super) fn on_memory_turn_start(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    persona: &Persona,
    user_content: &str,
    prefetched_count: usize,
    tool_count: usize,
) -> AppResult<()> {
    append_parent_phase_event(
        store,
        run_id,
        "memory_turn_started",
        json!({
            "provider": "builtin",
            "conversationId": conversation_id,
            "personaId": persona.id,
            "userChars": user_content.chars().count(),
            "prefetched": prefetched_count,
            "toolCount": tool_count
        }),
    )
}

pub(super) fn on_memory_turn_synced(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    persona: &Persona,
    user_content: &str,
    assistant_content: &str,
) -> AppResult<()> {
    let _ = sync_builtin_memory_markdown(store, persona);
    append_parent_phase_event(
        store,
        run_id,
        "memory_turn_synced",
        json!({
            "provider": "builtin",
            "conversationId": conversation_id,
            "personaId": persona.id,
            "userChars": user_content.chars().count(),
            "assistantChars": assistant_content.chars().count()
        }),
    )
}

pub(super) fn on_memory_write(
    store: &AppStore,
    run_id: &str,
    persona: &Persona,
    action: &str,
    target: &str,
    content: &str,
) -> AppResult<()> {
    let _ = sync_builtin_memory_markdown(store, persona);
    let mirrored = mirror_memory_write_to_holographic(store, action, target, content)?;
    if run_id.trim().is_empty() {
        return Ok(());
    }
    append_parent_phase_event(
        store,
        run_id,
        "memory_write_observed",
        json!({
            "provider": "builtin",
            "personaId": persona.id,
            "action": action,
            "target": target,
            "contentChars": content.chars().count(),
            "providerMirrors": [{
                "provider": "holographic",
                "mirrored": mirrored.is_some(),
                "factId": mirrored.as_ref().and_then(|fact| fact.get("id")).and_then(serde_json::Value::as_str)
            }]
        }),
    )
}

pub(super) fn memory_pre_compress_context(
    store: &AppStore,
    persona: &Persona,
    query: &str,
) -> AppResult<String> {
    let memories = builtin_memory_prefetch(store, persona, query)?;
    let provider_context = memory_provider_prompt_context(store, &memories, query)?;
    if provider_context.trim().is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "[assistant at memory-pre-compress] Memory provider pre-compress context to preserve if relevant: {}",
        provider_context.replace('\n', " ")
    ))
}

pub(super) fn memory_provider_prompt_context(
    store: &AppStore,
    memory_blocks: &[MemoryEntry],
    query: &str,
) -> AppResult<String> {
    let _ = ensure_hermes_memory_dir(store);
    let builtin_lines = memory_blocks.iter().map(|memory| {
        format!(
            "- [builtin:{} importance {}] {}",
            memory.id,
            memory.importance,
            sanitize_memory_context(&memory.summary)
        )
    });
    let holographic_lines = holographic_memory_prefetch_facts(store, query, 8)?
        .into_iter()
        .filter_map(|fact| {
            let content = fact.get("content").and_then(serde_json::Value::as_str)?;
            let id = fact
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let trust = fact
                .get("trust")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.5);
            Some(format!(
                "- [holographic:{id} trust {:.1}] {}",
                trust,
                sanitize_memory_context(content)
            ))
        });
    let raw_context = builtin_lines
        .chain(holographic_lines)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(build_memory_context_block(&raw_context))
}

pub(super) fn sanitize_memory_context(text: &str) -> String {
    let without_blocks = strip_tagged_blocks(text, MEMORY_OPEN_TAG, MEMORY_CLOSE_TAG);
    without_blocks
        .lines()
        .filter(|line| !is_internal_memory_note(line))
        .map(strip_memory_tags_from_line)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn memory_prefetch_score(memory: &MemoryEntry, query: &str) -> u32 {
    let importance = memory.importance as u32;
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return importance;
    }
    let text = memory.summary.to_ascii_lowercase();
    if text.contains(&query) {
        return 1000 + query.len() as u32 + importance;
    }
    let term_score = query
        .split_whitespace()
        .filter(|term| !term.is_empty() && text.contains(*term))
        .map(|term| 20 + term.len() as u32)
        .sum::<u32>();
    if term_score == 0 {
        return 0;
    }
    term_score + importance
}

pub(super) fn build_memory_context_block(raw_context: &str) -> String {
    if raw_context.trim().is_empty() {
        return String::new();
    }
    let clean = sanitize_memory_context(raw_context);
    if clean.trim().is_empty() {
        return String::new();
    }
    format!(
        "{MEMORY_OPEN_TAG}\n[System note: The following is recalled memory context, NOT new user input. Treat as authoritative reference data for the agent's persistent memory.]\n\n{clean}\n{MEMORY_CLOSE_TAG}"
    )
}

fn strip_tagged_blocks(text: &str, open_tag: &str, close_tag: &str) -> String {
    let mut remaining = text.to_string();
    loop {
        let lower = remaining.to_ascii_lowercase();
        let Some(open_idx) = lower.find(open_tag) else {
            return remaining;
        };
        let after_open = open_idx + open_tag.len();
        let Some(close_rel_idx) = lower[after_open..].find(close_tag) else {
            remaining.truncate(open_idx);
            return remaining;
        };
        let close_end = after_open + close_rel_idx + close_tag.len();
        remaining.replace_range(open_idx..close_end, "");
    }
}

fn strip_memory_tags_from_line(line: &str) -> String {
    line.replace(MEMORY_OPEN_TAG, "")
        .replace(MEMORY_CLOSE_TAG, "")
        .replace(&MEMORY_OPEN_TAG.to_ascii_uppercase(), "")
        .replace(&MEMORY_CLOSE_TAG.to_ascii_uppercase(), "")
}

fn is_internal_memory_note(line: &str) -> bool {
    let normalized = line.trim().to_ascii_lowercase();
    normalized.starts_with("[system note:")
        && normalized.contains("recalled memory context")
        && normalized.contains("not new user input")
}
