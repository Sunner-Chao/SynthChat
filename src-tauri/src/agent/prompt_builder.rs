use std::{
    process::{Command as StdCommand, Stdio},
    sync::OnceLock,
};

use crate::{
    error::{AppError, AppResult},
    model_catalog::model_capability_prompt_block,
    models::{
        AgentDefinition, MemoryEntry, Persona, ShortContextState, SkillPromptBlock, ToolDefinition,
    },
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
) -> String {
    let availability = internal_tool_availability(store);
    let model_metadata_block = agent_model_metadata_prompt_block(store, agent);
    agent_planner_prompt_for_agent_context_with_availability(
        observations,
        skill_blocks,
        memory_blocks,
        short_context,
        mcp_tools,
        tool_context,
        agent,
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
    let internal_tool_block = render_internal_tool_prompt_block(agent, tool_context, availability);
    let environment_probe_block = environment_probe_prompt_block();
    format!(
        r#"You are SynthChat's recovered agent runtime. Decide the next step from the user request and current observations.

Return JSON only. Do not wrap it in markdown.

Tool-use enforcement:
When tools are available and the task needs inspection, commands, file edits, browsing, or other action, take that action with a tool instead of describing what you would do. If you say you will inspect, run, create, edit, search, fetch, or test something, your next response must be the corresponding tool call. Do not end with a promise of future tool use.

Skill instructions:
{skill_block}

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
Use session_search when the user asks what happened earlier, asks to resume prior work, or needs evidence from previous conversations/runs/tool outputs.
Use clarify only when required information is missing and no safe tool action or partial answer can move the task forward.
Use cronjob only when the user asks to schedule, remind, recur, automate later, pause, resume, delete, list, or manually trigger scheduled work.
Use recall_memory when long-term persona facts or preferences may affect the answer and are not already visible. Use remember_fact only for stable user facts/preferences; do not store transient task notes. Use manage_memory replace/remove when the user corrects or invalidates an existing memory.
Use skills_list before skill_view when you need available skill names; use skill_view to load only the skill or linked file needed for the task. Use skill_manage only to create or refine reusable procedural knowledge after you understand the workflow.
For MCP/capability tools, use the listed tool name exactly and provide payload matching its schema.
Before write_file, patch, delete_file, or move_file, inspect the target file unless the user explicitly provided the full intended content. When modifying a file you just read, pass read_file's sha256/modifiedUnixMs back as expectedSha256/expectedModifiedUnixMs so stale edits fail instead of overwriting newer content.
Use terminal/process/execute_code only when command execution is necessary and the agent is configured to allow shell access.
Use workspace_diagnostics after code changes or when build/type/test failures are relevant; it runs bounded read-only diagnostics.
File tools can access only the configured agent workspace. If a requested local path is outside that workspace, explain the workspace limitation and ask the user to switch or configure the workspace instead of repeatedly retrying the same path.
When you create or identify a file the user should open/download, use artifact with action=publish_file for an existing workspace file or content for generated text, then mention the artifact path in the final answer.
Use web_extract when the user gives specific HTTP(S) URLs or after web_search when page content, documentation, article text, or source evidence is needed.
For web page tasks, prefer browser_snapshot/browser_navigate first for static pages and browser_cdp action=snapshot for dynamic pages; inspect forms, inputs, links, refs, and request clues before choosing click/type/fetch-style actions.
When enough context is available, return {{"action":"final","content":"your answer"}}.
If no tool is needed, answer directly with final.

Current observations:
{observation_block}"#
    )
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
            .arg(command)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    } else {
        StdCommand::new("sh")
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
    format!(
        "boundaryMessageId: {}\nsummaryMessages: {}\nsummaryTokens: {}\n{}",
        short_context.boundary_id.as_deref().unwrap_or("<none>"),
        short_context.summary_messages,
        short_context.summary_tokens,
        truncate_for_prompt(short_context.summary.trim(), 2000)
    )
}
