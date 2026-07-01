use std::{
    process::{Command as StdCommand, Stdio},
    sync::OnceLock,
};

use crate::{
    error::{AppError, AppResult},
    model_catalog::model_capability_prompt_block,
    models::{
        AgentDefinition, ChatConfig, MemoryEntry, Persona, ShortContextState, SkillPromptBlock,
        ToolDefinition,
    },
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{
    build_memory_context_block, builtin_memory_prefetch, holographic_memory_prefetch_facts,
    internal_tool_availability, render_internal_tool_prompt_block, render_mcp_tool_definitions,
    truncate_for_prompt, InternalToolAvailability, ToolExecutionContext,
};

#[allow(dead_code)]
pub(super) fn agent_planner_prompt(
    observations: &[String],
    skill_blocks: &[SkillPromptBlock],
    memory_blocks: &[MemoryEntry],
    short_context: &ShortContextState,
    mcp_tools: &[ToolDefinition],
) -> String {
    agent_planner_prompt_for_context(
        observations,
        skill_blocks,
        memory_blocks,
        short_context,
        mcp_tools,
        ToolExecutionContext::Interactive,
    )
}

#[allow(dead_code)]
pub(super) fn agent_planner_prompt_for_context(
    observations: &[String],
    skill_blocks: &[SkillPromptBlock],
    memory_blocks: &[MemoryEntry],
    short_context: &ShortContextState,
    mcp_tools: &[ToolDefinition],
    tool_context: ToolExecutionContext,
) -> String {
    let default_agent = AgentDefinition::default();
    agent_planner_prompt_for_agent_context(
        observations,
        skill_blocks,
        memory_blocks,
        short_context,
        mcp_tools,
        tool_context,
        &default_agent,
    )
}

pub(super) fn agent_planner_prompt_for_agent_context(
    observations: &[String],
    skill_blocks: &[SkillPromptBlock],
    memory_blocks: &[MemoryEntry],
    short_context: &ShortContextState,
    mcp_tools: &[ToolDefinition],
    tool_context: ToolExecutionContext,
    agent: &AgentDefinition,
) -> String {
    agent_planner_prompt_for_agent_context_with_availability(
        observations,
        skill_blocks,
        memory_blocks,
        short_context,
        mcp_tools,
        tool_context,
        agent,
        None,
        None,
        None,
        &InternalToolAvailability::all_available(),
        "Current LLM model metadata: unavailable.",
    )
}

pub(super) fn agent_planner_prompt_for_agent_context_with_store(
    store: &AppStore,
    observations: &[String],
    skill_blocks: &[SkillPromptBlock],
    memory_blocks: &[MemoryEntry],
    short_context: &ShortContextState,
    mcp_tools: &[ToolDefinition],
    tool_context: ToolExecutionContext,
    agent: &AgentDefinition,
    persona: Option<&Persona>,
) -> String {
    let availability = internal_tool_availability(store);
    let model_metadata_block = agent_model_metadata_prompt_block(store, agent);
    let user_profile_name = store
        .profile()
        .ok()
        .map(|profile| profile.name.trim().to_string())
        .filter(|name| !name.is_empty() && name != "用户");
    agent_planner_prompt_for_agent_context_with_availability(
        observations,
        skill_blocks,
        memory_blocks,
        short_context,
        mcp_tools,
        tool_context,
        agent,
        persona,
        user_profile_name.as_deref(),
        Some(store),
        &availability,
        &model_metadata_block,
    )
}

pub(super) fn agent_planner_prompt_for_agent_context_with_availability(
    observations: &[String],
    skill_blocks: &[SkillPromptBlock],
    memory_blocks: &[MemoryEntry],
    short_context: &ShortContextState,
    mcp_tools: &[ToolDefinition],
    tool_context: ToolExecutionContext,
    agent: &AgentDefinition,
    persona: Option<&Persona>,
    user_profile_name: Option<&str>,
    store: Option<&AppStore>,
    availability: &InternalToolAvailability,
    model_metadata_block: &str,
) -> String {
    let observation_block = if observations.is_empty() {
        "No tool observations yet.".to_string()
    } else {
        observations.join("\n\n")
    };
    let skill_block = render_skill_prompt_blocks(skill_blocks);
    let memory_block = render_memory_prompt_blocks(memory_blocks);
    let short_context_block = render_short_context_block(short_context);
    let mcp_tool_block = render_mcp_tool_definitions(mcp_tools);
    let internal_tool_block =
        render_internal_tool_prompt_block(agent, tool_context, availability, store);
    let environment_probe_block = environment_probe_prompt_block();
    let persona_block = render_persona_prompt_block(persona, user_profile_name);
    let delegation_strategy_block = delegation_strategy_prompt_block(store);
    format!(
        r#"You are SynthChat's recovered agent runtime. Decide the next step from the user request and current observations.

Return JSON only. Do not wrap it in markdown.

Tool-use enforcement:
When tools are available and the task needs inspection, commands, file edits, browsing, or other action, take that action with a tool instead of describing what you would do. If you say you will inspect, run, create, edit, search, fetch, or test something, your next response must be the corresponding tool call. Do not end with a promise of future tool use.

Multi-agent collaboration:
{delegation_strategy_block}

Skill instructions:
{skill_block}

Current persona:
{persona_block}

Relevant memory:
{memory_block}

Conversation summary:
{short_context_block}

Available MCP/capability tools:
{mcp_tool_block}

Available internal tools:
{internal_tool_block}

Model metadata:
{model_metadata_block}

Environment notes:
{environment_probe_block}

Use tools when the answer needs project context. Prefer search_files before read_file when you do not know the exact file.
When the user explicitly asks you to check, query, inspect, run a tool, or needs current local environment information such as the current date/time, you must use an available tool before the final answer. Do not claim you cannot check if terminal/env_probe or another relevant tool is available.
Use session_search when the user asks what happened earlier, asks to resume prior work, needs evidence from previous conversations/runs/tool outputs, or needs deleted-session summaries saved as session memory.
Use clarify only when required information is missing and no safe tool action or partial answer can move the task forward.
Use cronjob only when the user asks to schedule, remind, recur, automate later, pause, resume, delete, list, or manually trigger scheduled work.
Use recall_memory when long-term persona facts or preferences may affect the answer and are not already visible. Use remember_fact only for stable user facts/preferences; do not store transient task notes. Use manage_memory replace/remove when the user corrects or invalidates an existing memory.
Use skills_list before skill_view when you need available skill names; use skill_view to load only the skill or linked file needed for the task. Use skill_manage only to create or refine reusable procedural knowledge after you understand the workflow.
For MCP/capability tools, use the listed tool name exactly and provide payload matching its schema.
Before write_file, patch, delete_file, or move_file, inspect the target file unless the user explicitly provided the full intended content. When modifying a file you just read, pass read_file's sha256/modifiedUnixMs back as expectedSha256/expectedModifiedUnixMs so stale edits fail instead of overwriting newer content.
Use terminal/process/execute_code only when command execution is necessary and the agent is configured to allow shell access.
Use workspace_diagnostics after code changes or when build/type/test failures are relevant; it runs bounded read-only diagnostics.
Direct file tools may access absolute local paths when permitted, as well as paths relative to the configured agent workspace. Do not claim that you can only access workspace files. If a file tool rejects a path, explain the concrete tool error and choose an appropriate available alternative only when needed.
When you create or identify a file the user should open/download, use document for common documents (docx/xlsx/pptx/html/md/txt/csv) or artifact with action=publish_file for an existing workspace file, then mention the artifact path in the final answer. If the user asks to send a document to the linked WeChat mobile side, include the tool's returned mediaTag (MEDIA:<path>) as its own line in the final answer so the WeChat bridge silently uploads it as a file and hides the directive from visible text.
Use web_extract when the user gives specific HTTP(S) URLs or after web_search when page content, documentation, article text, or source evidence is needed.
For web page tasks, prefer browser_snapshot/browser_navigate first for static pages and browser_cdp action=snapshot for dynamic pages; inspect forms, inputs, links, refs, and request clues before choosing click/type/fetch-style actions.
When enough context is available, return {{"action":"final","content":"your answer"}}.
If no tool is needed, answer directly with final.

Current observations:
{observation_block}"#
    )
}

fn delegation_strategy_prompt_block(store: Option<&AppStore>) -> String {
    let strategy = store
        .and_then(|store| store.config().ok())
        .map(|config| normalize_delegation_strategy(&config.chat.delegation_strategy))
        .unwrap_or_else(|| {
            normalize_delegation_strategy(&ChatConfig::default().delegation_strategy)
        });
    match strategy.as_str() {
        "single_agent_chat" => {
            "Current strategy: single_agent_chat. Prefer direct main-agent execution for normal work. Use delegate_task only when the user explicitly asks for subagents, an isolated second opinion is needed, or the same direct path has failed and a focused diagnostic child can unblock it. If you do delegate, keep children leaf-only and synthesize their result before finalizing.".into()
        }
        "router_specialists" => {
            "Current strategy: router_specialists. Classify the task into concrete specialties first, then route only the needed specialty work to focused delegate_task children such as researcher, planner, coder, reviewer, or debugger. Avoid broad duplicate children; each child should receive a precise goal, relevant context, and narrowed toolsets. The main agent synthesizes specialist outputs and resolves conflicts.".into()
        }
        "planner_executor" => {
            "Current strategy: planner_executor. For implementation, debugging, research, and recovery tasks, plan before executing: decompose the objective, delegate execution or verification subtasks to focused children, then validate and synthesize. Prefer at least one executor/reviewer split when file edits or repeated tool calls are likely. Small single-step answers may still run directly.".into()
        }
        "supervisor_dynamic" => {
            "Current strategy: supervisor_dynamic. Act as a supervisor that assigns work dynamically, monitors child results, and spawns follow-up delegate_task children only when new evidence justifies it. Use role=orchestrator and canDelegate=true only when nested delegation is enabled by current limits; otherwise keep children as leaf workers. Do not retry a failed direct workflow before delegating a focused diagnostic or recovery child.".into()
        }
        "peer_handoff" => {
            "Current strategy: peer_handoff. Use sequential peer handoffs for non-trivial work: one child proposes or investigates, another critiques or verifies, and the main agent resolves disagreements before acting or finalizing. Include the previous peer's findings in the next child context. Keep handoffs short and evidence-based.".into()
        }
        "mixture_consensus" => {
            "Current strategy: mixture_consensus. For hard reasoning, ambiguous design choices, or answer quality comparisons, prefer mixture_of_agents to gather multiple model perspectives and synthesize. For tool-heavy local tasks, use delegate_task for focused execution and optionally mixture_of_agents for final reasoning or review. The main agent remains responsible for verification.".into()
        }
        _ => {
            "Current strategy: auto. Small single-step tasks may run directly. For complex implementation, debugging, review, research, recovery, broad code inspection, terminal work, or file edits, delegate focused subagents before deep main-agent execution, then synthesize and verify. If a recent attempt failed, looped, or produced conflicting evidence, do not immediately retry the same direct workflow; delegate a focused diagnostic or implementation child with the failure context first.".into()
        }
    }
}

fn normalize_delegation_strategy(value: &str) -> String {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "single_agent" | "single" | "direct" | "single_agent_chat" => "single_agent_chat".into(),
        "router" | "specialists" | "router_specialists" => "router_specialists".into(),
        "planner" | "executor" | "planner_executor" => "planner_executor".into(),
        "supervisor" | "dynamic" | "supervisor_dynamic" => "supervisor_dynamic".into(),
        "peer" | "handoff" | "peer_handoff" => "peer_handoff".into(),
        "mixture" | "moa" | "consensus" | "mixture_consensus" => "mixture_consensus".into(),
        _ => "auto".into(),
    }
}

fn render_persona_prompt_block(
    persona: Option<&Persona>,
    user_profile_name: Option<&str>,
) -> String {
    let Some(persona) = persona else {
        return user_profile_name
            .map(|name| format!("User profile: The user's preferred display name is {name}."))
            .unwrap_or_else(|| "No persona context provided.".into());
    };
    let mut parts = Vec::new();
    if !persona.name.trim().is_empty() {
        parts.push(format!("Name: {}", persona.name.trim()));
    }
    if !persona.system_prompt.trim().is_empty() {
        parts.push(format!("System prompt:\n{}", persona.system_prompt.trim()));
    }
    if !persona.system_instructions.trim().is_empty() {
        parts.push(format!(
            "System instructions:\n{}",
            persona.system_instructions.trim()
        ));
    }
    if !persona.character_prompt.trim().is_empty() {
        parts.push(format!(
            "Character profile:\n{}",
            persona.character_prompt.trim()
        ));
    }
    if !persona.output_examples.trim().is_empty() {
        parts.push(format!(
            "Output examples:\n{}",
            persona.output_examples.trim()
        ));
    }
    if let Some(name) = user_profile_name {
        parts.push(format!(
            "User profile: The user's preferred display name is {name}."
        ));
    }
    if parts.is_empty() {
        "No persona context provided.".into()
    } else {
        truncate_for_prompt(&parts.join("\n\n"), 24_000)
    }
}

fn agent_model_metadata_prompt_block(store: &AppStore, agent: &AgentDefinition) -> String {
    let provider = store
        .provider(if agent.llm_provider.trim().is_empty() {
            None
        } else {
            Some(agent.llm_provider.trim())
        })
        .ok()
        .map(|mut provider| {
            if !agent.llm_model.trim().is_empty() {
                provider.model = agent.llm_model.trim().to_string();
            }
            provider
        });
    match provider {
        Some(provider) => model_capability_prompt_block(&provider),
        None => "Current LLM model metadata: unavailable.".into(),
    }
}

fn environment_probe_prompt_block() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    let line = CACHE.get_or_init(build_environment_probe_line);
    if line.trim().is_empty() {
        "No notable local environment caveats detected.".into()
    } else {
        line.clone()
    }
}

fn build_environment_probe_line() -> String {
    let py3_ver = python_version_of("python3");
    let py_ver = python_version_of("python");
    let py3_has_pip = py3_ver
        .as_ref()
        .map(|_| has_pip_module("python3"))
        .unwrap_or(false);
    let pip_bound_to = pip_python_version();
    let py3_pep668 = py3_ver
        .as_ref()
        .map(|_| detect_pep668("python3"))
        .unwrap_or(false);
    let has_uv = command_exists("uv");
    let mismatch = pip_bound_to
        .as_ref()
        .zip(py3_ver.as_ref())
        .map(|(pip, py3)| !py3.starts_with(pip))
        .unwrap_or(false);
    if py3_ver.is_some() && py3_has_pip && !mismatch && (!py3_pep668 || has_uv) {
        return String::new();
    }
    let mut bits = Vec::new();
    if let Some(py3_ver) = py3_ver.as_deref() {
        let mut item = format!("python3={py3_ver}");
        if !py3_has_pip {
            item.push_str(" (no pip module)");
        }
        bits.push(item);
    } else {
        bits.push("python3=missing".into());
    }
    if let Some(py_ver) = py_ver.as_deref() {
        if py3_ver.as_deref() != Some(py_ver) {
            bits.push(format!("python={py_ver}"));
        }
    } else if py3_ver.is_some() {
        bits.push("python=missing (use python3)".into());
    }
    if let Some(pip) = pip_bound_to.as_deref() {
        if mismatch {
            bits.push(format!("pip->python{pip} (mismatch)"));
        } else if !py3_has_pip {
            bits.push(format!("pip->python{pip}"));
        }
    } else if !py3_has_pip {
        bits.push("pip=missing".into());
    }
    if py3_pep668 {
        bits.push("PEP 668=yes (use venv or uv)".into());
    }
    if has_uv {
        bits.push("uv=installed".into());
    }
    if bits.is_empty() {
        String::new()
    } else {
        format!("Python toolchain: {}.", bits.join(", "))
    }
}

fn command_exists(command: &str) -> bool {
    if cfg!(windows) {
        StdCommand::new("where")
            .hide_window()
            .arg(command)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    } else {
        StdCommand::new("sh")
            .hide_window()
            .arg("-c")
            .arg(format!("command -v {}", shell_escape_single(command)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn python_version_of(binary: &str) -> Option<String> {
    if !command_exists(binary) {
        return None;
    }
    run_probe_command(
        binary,
        &[
            "-c",
            "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}')",
        ],
    )
    .ok()
}

fn has_pip_module(binary: &str) -> bool {
    if !command_exists(binary) {
        return false;
    }
    StdCommand::new(binary)
        .hide_window()
        .args(["-m", "pip", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn detect_pep668(binary: &str) -> bool {
    if !command_exists(binary) {
        return false;
    }
    run_probe_command(
        binary,
        &[
            "-c",
            "import os; marker=os.path.join(os.path.dirname(os.__file__), 'EXTERNALLY-MANAGED'); print('yes' if os.path.exists(marker) else 'no')",
        ],
    )
    .map(|output| output.trim() == "yes")
    .unwrap_or(false)
}

fn pip_python_version() -> Option<String> {
    if !command_exists("pip") {
        return None;
    }
    let output = run_probe_command("pip", &["--version"]).ok()?;
    let tail = output.rsplit("(python ").next()?;
    output
        .contains("(python ")
        .then(|| tail.trim_end_matches(')').trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_probe_command(command: &str, args: &[&str]) -> AppResult<String> {
    let output = StdCommand::new(command)
        .hide_window()
        .args(args)
        .output()
        .map_err(|error| AppError::BadRequest(format!("probe command failed: {error}")))?;
    if !output.status.success() {
        return Err(AppError::BadRequest("probe command failed".into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn shell_escape_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_skill_prompt_blocks(skill_blocks: &[SkillPromptBlock]) -> String {
    if skill_blocks.is_empty() {
        return "No enabled or explicitly requested skill instructions.".into();
    }
    skill_blocks
        .iter()
        .map(|block| {
            format!(
                "### Skill: {} ({})\n{}",
                block.name,
                block.id,
                block.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn memory_prompt_blocks(
    store: &AppStore,
    persona: &Persona,
) -> AppResult<Vec<MemoryEntry>> {
    memory_prompt_blocks_for_query(store, persona, "")
}

pub(super) fn memory_prompt_blocks_for_query(
    store: &AppStore,
    persona: &Persona,
    query: &str,
) -> AppResult<Vec<MemoryEntry>> {
    let mut memories = builtin_memory_prefetch(store, persona, query)?;
    for fact in holographic_memory_prefetch_facts(store, query, 8)? {
        let Some(content) = fact.get("content").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let id = fact
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("fact");
        let trust = fact
            .get("trust")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.5);
        memories.push(MemoryEntry {
            id: format!("holographic:{id}"),
            persona_id: persona.id.clone(),
            target: "memory".into(),
            summary: format!("[Holographic fact trust {:.1}] {}", trust, content.trim()),
            importance: ((trust * 5.0).round() as u8).clamp(1, 5),
            created_at: fact
                .get("createdAt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            updated_at: fact
                .get("updatedAt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(memories)
}

fn render_memory_prompt_blocks(memory_blocks: &[MemoryEntry]) -> String {
    if memory_blocks.is_empty() {
        return "No prompt-safe persona memory is available.".into();
    }
    let raw_context = memory_blocks
        .iter()
        .map(|memory| {
            format!(
                "- importance {} · {}",
                memory.importance,
                truncate_for_prompt(memory.summary.trim(), 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let fenced = build_memory_context_block(&raw_context);
    if fenced.is_empty() {
        "No prompt-safe persona memory is available.".into()
    } else {
        fenced
    }
}

fn render_short_context_block(short_context: &ShortContextState) -> String {
    if short_context.summary.trim().is_empty() {
        return "No compacted conversation summary is available.".into();
    }
    let summary = prompt_safe_short_context_summary(short_context.summary.trim());
    format!(
        "boundaryMessageId: {}\nsummaryMessages: {}\nsummaryTokens: {}\n{}",
        short_context.boundary_id.as_deref().unwrap_or("<none>"),
        short_context.summary_messages,
        short_context.summary_tokens,
        truncate_for_prompt(&summary, 2000)
    )
}

fn prompt_safe_short_context_summary(summary: &str) -> String {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("deterministic fallback") {
        return summary.to_string();
    }
    let mut lines = vec![
        "[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted by a deterministic fallback summary. The fallback's active-task fields are suppressed because they may be stale.".to_string(),
        "## Active Task".to_string(),
        "None recoverable from deterministic fallback. Use only the latest visible user message and current repository/session state.".to_string(),
        "## Pending User Asks".to_string(),
        "None recoverable from deterministic fallback.".to_string(),
        "## Remaining Work".to_string(),
        "Determine remaining work from the latest visible user message; do not infer active work from compacted historical requests.".to_string(),
    ];
    let relevant_files = extract_summary_section(summary, "## Relevant Files")
        .map(|section| {
            section
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    trimmed.starts_with("- ") && !trimmed.contains("User asked:")
                })
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|section| !section.trim().is_empty())
        .unwrap_or_else(|| "None safely recoverable.".into());
    lines.push("## Relevant Files".into());
    lines.push(relevant_files);
    lines.push("## Critical Context".into());
    lines.push("Existing deterministic fallback summaries may contain stale requests such as old Active Task, In Progress, or Pending User Asks entries; ignore those fields.".into());
    lines.join("\n")
}

fn extract_summary_section<'a>(summary: &'a str, heading: &str) -> Option<&'a str> {
    let start = summary.find(heading)?;
    let rest = &summary[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(rest[..end].trim())
}
