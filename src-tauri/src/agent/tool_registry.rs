use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    env,
    hash::{Hash, Hasher},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, tool_event_kind, AgentDefinition, BrowserProvider, McpCallResult, ToolDefinition,
        ToolEvent,
    },
    store::AppStore,
};

use super::{
    apply_agent_toolset_policy, apply_tool_context_policy, call_mcp_tool_with_retry,
    decision_parser::{
        parse_tool_arguments_json, provider_tool_call_id, PROVIDER_TOOL_CALL_META_KEY,
    },
    discord_settings, feishu_settings, homeassistant_settings, is_risky_tool_call,
    list_python_plugin_tools, provider_api_key, qweather_settings, redact_json_value,
    redact_sensitive_text, run_post_tool_call_hooks, run_pre_tool_call_hooks,
    run_python_plugin_tool, run_transform_tool_result_hooks, spotify_settings, summarize_tool_text,
    tool_allowed_by_agent_capabilities, tool_allowed_by_agent_toolsets, tool_allowed_in_context,
    yuanbao_bridge_available, yuanbao_stickers_available, ToolExecutionContext,
};

const PYTHON_PLUGIN_SERVER_PREFIX: &str = "__python_plugin:";

pub(super) fn render_internal_tool_prompt_block(
    agent: &AgentDefinition,
    context: ToolExecutionContext,
    availability: &InternalToolAvailability,
) -> String {
    internal_tool_prompt_lines()
        .into_iter()
        .filter(|(name, _)| {
            if !internal_tool_available(name, availability) {
                return false;
            }
            let tool = ToolDefinition {
                name: (*name).into(),
                display_name: (*name).into(),
                description: String::new(),
                source: "internal".into(),
                server_id: "__internal".into(),
                tool_name: (*name).into(),
                input_schema: json!({}),
                requires_approval: false,
            };
            tool_allowed_in_context(&tool, context)
                && tool_allowed_by_agent_capabilities(&tool, agent)
                && tool_allowed_by_agent_toolsets(&tool, agent)
        })
        .map(|(name, line)| internal_tool_prompt_line_for_agent(name, line, agent))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) struct InternalToolAvailability {
    browser_session_provider: bool,
    search_provider: bool,
    image_provider: bool,
    video_provider: bool,
    vision_provider: bool,
    audio_provider: bool,
    weather: bool,
    homeassistant: bool,
    feishu: bool,
    yuanbao_bridge: bool,
    yuanbao_stickers: bool,
    spotify: bool,
    discord: bool,
}

const TOOL_AVAILABILITY_CACHE_TTL: Duration = Duration::from_secs(30);
static INTERNAL_TOOL_AVAILABILITY_CACHE: OnceLock<Mutex<Option<CachedInternalToolAvailability>>> =
    OnceLock::new();

#[derive(Clone)]
struct CachedInternalToolAvailability {
    fingerprint: u64,
    captured_at: Instant,
    availability: InternalToolAvailability,
}

impl InternalToolAvailability {
    pub(super) fn all_available() -> Self {
        Self {
            browser_session_provider: true,
            search_provider: true,
            image_provider: true,
            video_provider: true,
            vision_provider: true,
            audio_provider: true,
            weather: true,
            homeassistant: true,
            feishu: true,
            yuanbao_bridge: true,
            yuanbao_stickers: true,
            spotify: true,
            discord: true,
        }
    }
}

impl Clone for InternalToolAvailability {
    fn clone(&self) -> Self {
        Self {
            browser_session_provider: self.browser_session_provider,
            search_provider: self.search_provider,
            image_provider: self.image_provider,
            video_provider: self.video_provider,
            vision_provider: self.vision_provider,
            audio_provider: self.audio_provider,
            weather: self.weather,
            homeassistant: self.homeassistant,
            feishu: self.feishu,
            yuanbao_bridge: self.yuanbao_bridge,
            yuanbao_stickers: self.yuanbao_stickers,
            spotify: self.spotify,
            discord: self.discord,
        }
    }
}

pub(super) fn internal_tool_availability(store: &AppStore) -> InternalToolAvailability {
    let fingerprint = internal_tool_availability_fingerprint(store);
    let cache = INTERNAL_TOOL_AVAILABILITY_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.fingerprint == fingerprint
                && cached.captured_at.elapsed() < TOOL_AVAILABILITY_CACHE_TTL
            {
                return cached.availability.clone();
            }
        }
    }
    let availability = compute_internal_tool_availability(store);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedInternalToolAvailability {
            fingerprint,
            captured_at: Instant::now(),
            availability: availability.clone(),
        });
    }
    availability
}

fn compute_internal_tool_availability(store: &AppStore) -> InternalToolAvailability {
    let config = store.config().ok();
    InternalToolAvailability {
        browser_session_provider: store
            .browser_providers()
            .ok()
            .is_some_and(|providers| hermes_browser_session_provider_available(&providers)),
        search_provider: store
            .search_providers()
            .ok()
            .is_some_and(|providers| providers.iter().any(search_provider_configured)),
        image_provider: store.enabled_image_provider().ok().flatten().is_some(),
        video_provider: store.enabled_video_provider().ok().flatten().is_some(),
        vision_provider: store.enabled_vision_provider().ok().flatten().is_some(),
        audio_provider: store
            .providers()
            .map(|providers| {
                providers.iter().any(|provider| {
                    provider.enabled
                        && provider.provider_type.trim() != "echo"
                        && !provider.base_url.trim().is_empty()
                })
            })
            .unwrap_or(false),
        weather: config
            .as_ref()
            .map(|config| qweather_settings(&config.weather).is_ok())
            .unwrap_or(false),
        homeassistant: config
            .as_ref()
            .map(|config| homeassistant_settings(&config.homeassistant).is_ok())
            .unwrap_or(false),
        feishu: config
            .as_ref()
            .map(|config| feishu_settings(&config.feishu).is_ok())
            .unwrap_or(false),
        yuanbao_bridge: config
            .as_ref()
            .map(|config| yuanbao_bridge_available(&config.yuanbao))
            .unwrap_or(false),
        yuanbao_stickers: config
            .as_ref()
            .map(|config| yuanbao_stickers_available(&config.yuanbao))
            .unwrap_or(false),
        spotify: config
            .as_ref()
            .map(|config| spotify_settings(&config.spotify).is_ok())
            .unwrap_or(false),
        discord: config
            .as_ref()
            .map(|config| discord_settings(&config.discord).is_ok())
            .unwrap_or(false),
    }
}

fn hermes_browser_session_provider_available(providers: &[BrowserProvider]) -> bool {
    ["browser-use", "browserbase"].iter().any(|legacy| {
        providers.iter().any(|provider| {
            browser_provider_matches_name(provider, legacy) && browser_provider_available(provider)
        })
    })
}

fn browser_provider_available(provider: &BrowserProvider) -> bool {
    provider.enabled
        && !provider.provider_type.trim().is_empty()
        && !provider.base_url.trim().is_empty()
        && provider_api_key(&provider.api_key, &provider.api_key_env)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn browser_provider_matches_name(provider: &BrowserProvider, name: &str) -> bool {
    let name = normalize_browser_provider_name(name);
    [
        provider.id.as_str(),
        provider.name.as_str(),
        provider.provider_type.as_str(),
    ]
    .iter()
    .any(|candidate| normalize_browser_provider_name(candidate) == name)
}

fn normalize_browser_provider_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

fn search_provider_configured(provider: &crate::models::SearchProvider) -> bool {
    if !provider.enabled {
        return false;
    }
    if !provider.base_url.trim().is_empty()
        && matches!(
            provider.provider_type.trim().to_ascii_lowercase().as_str(),
            "" | "searxng" | "searx"
        )
    {
        return true;
    }
    provider_api_key(&provider.api_key, &provider.api_key_env).is_some()
        || default_search_provider_env_key(&provider.provider_type)
            .and_then(|key| env::var(key).ok())
            .is_some_and(|value| !value.trim().is_empty())
}

fn default_search_provider_env_key(provider_type: &str) -> Option<&'static str> {
    match provider_type
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .as_str()
    {
        "firecrawl" => Some("FIRECRAWL_API_KEY"),
        "tavily" => Some("TAVILY_API_KEY"),
        "exa" => Some("EXA_API_KEY"),
        "brave-free" => Some("BRAVE_SEARCH_API_KEY"),
        "parallel" => Some("PARALLEL_API_KEY"),
        _ => None,
    }
}

fn internal_tool_availability_fingerprint(store: &AppStore) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_store_value(&mut hasher, "config", store.config().ok());
    hash_store_value(&mut hasher, "llm_providers", store.providers().ok());
    hash_store_value(&mut hasher, "image_providers", store.image_providers().ok());
    hash_store_value(&mut hasher, "video_providers", store.video_providers().ok());
    hash_store_value(
        &mut hasher,
        "vision_providers",
        store.vision_providers().ok(),
    );
    hash_store_value(
        &mut hasher,
        "search_providers",
        store.search_providers().ok(),
    );
    hash_store_value(
        &mut hasher,
        "browser_providers",
        store.browser_providers().ok(),
    );
    hasher.finish()
}

fn hash_store_value<T: serde::Serialize>(
    hasher: &mut DefaultHasher,
    label: &str,
    value: Option<T>,
) {
    label.hash(hasher);
    match value.and_then(|value| serde_json::to_string(&value).ok()) {
        Some(serialized) => serialized.hash(hasher),
        None => "<unavailable>".hash(hasher),
    }
}

pub(super) fn internal_tool_available(
    tool_name: &str,
    availability: &InternalToolAvailability,
) -> bool {
    match tool_name {
        "browser_create_session" | "browser_close_session" => availability.browser_session_provider,
        "web_provider" => true,
        "web_search" | "x_search" => availability.search_provider,
        "image_generate" => availability.image_provider,
        "video_generate" => availability.video_provider,
        "vision_analyze" | "video_analyze" | "browser_vision" => availability.vision_provider,
        "text_to_speech" | "transcribe_audio" => availability.audio_provider,
        "weather" => availability.weather,
        "ha_list_entities" | "ha_get_state" | "ha_list_services" | "ha_call_service" => {
            availability.homeassistant
        }
        "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment" => availability.feishu,
        "yb_query_group_info" | "yb_query_group_members" | "yb_send_dm" | "yb_send_sticker" => {
            availability.yuanbao_bridge
        }
        "yb_search_sticker" => availability.yuanbao_bridge || availability.yuanbao_stickers,
        "spotify_playback" | "spotify_devices" | "spotify_queue" | "spotify_search"
        | "spotify_playlists" | "spotify_albums" | "spotify_library" => availability.spotify,
        "discord" | "discord_admin" => availability.discord,
        _ => true,
    }
}

pub(super) fn internal_tool_prompt_lines() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "tool_search",
            r#"- tool_search: payload {"query":"tool capability to find","limit":8} searches available internal and MCP tools."#,
        ),
        (
            "tool_describe",
            r#"- tool_describe: payload {"name":"tool_name"} returns the tool description, payload shape, and schema if available."#,
        ),
        (
            "tool_call",
            r#"- tool_call: payload {"name":"tool_name","arguments":{}} invokes an available tool by name after tool_search/tool_describe discovery."#,
        ),
        (
            "read_file",
            r#"- read_file: payload {"path":"relative/or/absolute/path","offset":1,"limit":500} reads line-numbered pages and returns sha256/modifiedUnixMs file state; use {"mode":"raw","maxChars":80000} for unnumbered full text or {"mode":"chars","charOffset":0,"charLimit":12000} for character slices."#,
        ),
        (
            "file_state",
            r#"- file_state: payload {"action":"register|check|remove|writes_since","path":"relative/or/absolute/path","actor":"optional","since":"ISO timestamp","readerRunId":"optional"}. Hermes-style file state coordination: register records the current sha256/modifiedUnixMs for this run, check reports whether the file changed since the registered state, remove forgets a path, and writes_since lists sibling writes after a timestamp."#,
        ),
        (
            "search_files",
            r#"- search_files: payload {"query":"text","path":".","target":"content|files","fileGlob":"*.rs","limit":20,"offset":0,"outputMode":"content|files_only|count","context":0,"maxFiles":3000}"#,
        ),
        (
            "write_file",
            r#"- write_file: payload {"path":"relative/or/absolute/path","content":"complete file content","expectedSha256":"optional sha256 from read_file","expectedModifiedUnixMs":123}. Include expected state when overwriting a file you read."#,
        ),
        (
            "delete_file",
            r#"- delete_file: payload {"path":"relative/or/absolute/path","expectedSha256":"optional sha256 from read_file","expectedModifiedUnixMs":123} deletes a workspace file."#,
        ),
        (
            "move_file",
            r#"- move_file: payload {"src":"relative/or/absolute/source","dst":"relative/or/absolute/destination","expectedSha256":"optional source sha256","expectedModifiedUnixMs":123} moves or renames a workspace file."#,
        ),
        (
            "patch",
            r#"- patch: replace payload {"path":"relative/or/absolute/path","search":"exact old text","replace":"new text","replaceAll":false,"expectedSha256":"optional sha256 from read_file","expectedModifiedUnixMs":123} or {"path":"...","replacements":[{"search":"old","replace":"new"}]}; V4A payload {"mode":"patch","patch":"*** Begin Patch\n*** Update File: path\n@@\n-old\n+new\n*** End Patch","expectedFileStates":{"path":{"expectedSha256":"sha","expectedModifiedUnixMs":123}}} supports multi-file Add/Update/Delete/Move."#,
        ),
        (
            "terminal",
            r#"- terminal: payload {"command":"shell command","cwd":".","stdin":"optional stdin text","taskId":"optional session","sessionId":"optional session","timeoutSeconds":60,"background":false,"notify_on_complete":false,"watch_patterns":["ready"]}. With taskId/sessionId and no explicit cwd, SynthChat persists the shell CWD between terminal calls using a Hermes-style cwd marker. With background=true/backgroundProcess=true/bg=true, terminal is routed to process(action="start") so it returns a managed process session_id and supports notify_on_complete/watch_patterns, logs, wait, stdin, stop/kill, and notifications. Set TERMINAL_ENV=docker to run through the Docker backend with workspace and configured credential/skill/cache mounts, resource/security args, configured volumes/env, persistent labeled containers, cross-process reuse, and orphan cleanup. Set TERMINAL_ENV=singularity to execute through apptainer/singularity exec with workspace and configured credential/skill/cache bind mounts. Set TERMINAL_ENV=ssh with TERMINAL_SSH_HOST/USER/PORT/KEY to execute over SSH with stdin, timeout, ControlMaster reuse, remote cwd markers, credential/skill/cache upload sync unless TERMINAL_SSH_SYNC_FILES=false, and execution-time sync-back unless TERMINAL_SSH_SYNC_BACK=false; multi-file upload and sync-back use tar-over-SSH by default with scp fallback when disabled/unavailable, and stale synced remote files are removed unless TERMINAL_SSH_SYNC_DELETE=false. Set TERMINAL_ENV=modal with TERMINAL_MODAL_MODE=direct plus Modal credentials and the Python modal SDK for direct Modal sandbox execution with session cwd, app-data persisted snapshot restore/save, stale snapshot fallback to the base image, credential/skill/cache upload sync unless TERMINAL_MODAL_SYNC_FILES=false, and execution-time sync-back unless TERMINAL_MODAL_SYNC_BACK=false; set TERMINAL_MODAL_MODE=managed with a configured managed tool gateway/token for gateway-owned Modal terminal execution with remote cwd and environment snapshots. Set TERMINAL_ENV=daytona with DAYTONA_API_KEY and the Python daytona SDK for a basic persistent Daytona sandbox execution backend with credential/skill/cache upload sync unless TERMINAL_DAYTONA_SYNC_FILES=false and execution-time sync-back unless TERMINAL_DAYTONA_SYNC_BACK=false."#,
        ),
        (
            "process",
            r#"- process: payload {"action":"environment|environment_cleanup|checkpoint|recover|start|list|count|active|has_active|state|poll|log|wait|write|submit|close|stop|kill|stop_all|kill_all","command":"shell command","cwd":".","label":"dev server","processId":"...","taskId":"optional terminal session","sessionId":"...","session_id":"...","conversationId":"optional filter","runId":"optional filter","backend":"optional backend filter","envType":"optional env filter","data":"stdin text","timeoutSeconds":60,"offset":0,"limit":200,"forget":false,"notifyOnComplete":false,"watchPatterns":["ready"],"deleteSandbox":false}. environment reports Hermes-style TERMINAL_* backend config, requirements, remote sync files, local/SSH/Modal/Daytona terminal sessions, Modal persisted snapshot count, and Docker container lifecycle state; environment_cleanup stops matching SSH/Docker/Singularity/Modal/Daytona managed processes before tearing down backend state, runs SSH sync-back when active, stops or deletes Daytona sandboxes when TERMINAL_ENV=daytona, clears local/SSH/Modal/Daytona terminal state, clears Modal persisted snapshots, clears SSH sync state, and removes labeled Docker terminal containers for taskId/sessionId or all sessions. checkpoint reports and refreshes the Hermes-style processes.json metadata checkpoint for running managed processes; recover probes host PIDs and sandbox status_command entries, restoring live detached sessions that can be listed, polled, logged, killed, and reattached to the detached watcher. start/list/count/poll/log/wait/write/submit/close/stop/kill manage background processes; with TERMINAL_ENV=ssh, start launches via remote nohup and tracks a detached sandbox PID with status/kill/log tail over SSH, remote log cleanup on stop, but no stdin. With TERMINAL_ENV=docker, start reuses the labeled persistent Docker terminal container, launches via nohup in the container, and tracks a detached sandbox PID with docker exec status/kill/log tail and remote log cleanup, but no stdin. With TERMINAL_ENV=singularity, start launches a dedicated apptainer/singularity instance, runs nohup inside instance://..., and tracks a detached sandbox PID with exec status/kill/log tail and instance cleanup, but no stdin. With TERMINAL_ENV=modal, start creates a direct Modal sandbox, uploads configured sync files, launches via nohup, and tracks a detached sandbox PID with Modal SDK status/kill/log tail and sandbox cleanup, but no stdin. With TERMINAL_ENV=daytona, start creates or resumes the persistent Daytona sandbox, uploads configured sync files, launches via nohup, and tracks a detached sandbox PID with Daytona SDK status/kill/log tail and remote log cleanup, but no stdin. Detached SSH/Docker/Singularity/Modal/Daytona starts, explicit recover, and startup recovery attach one deduplicated Hermes-style poller per process id; the poller tails sandbox logs every ~2s, reads exit_command/exit file for sandbox exit codes, emits watch_match/watch_disabled events, and emits completed when notifyOnComplete=true or watch was disabled; startup reattach emits watchers_reattached. list and count return runningCount/running_count, exitedCount/exited_count, and hasActive/has_active, with taskId/sessionId, conversationId, runId, backend, and envType filters. stop_all/kill_all terminate all running managed processes matching taskId/sessionId, conversationId, runId, backend, or envType and emit one stopped event per process. list returns {"processes":[...],"count":N}; finished processes expose finishedAt/finished_at, are retained for about 30 minutes, and oldest finished entries are pruned once the registry exceeds 64 processes. kill is a Hermes-compatible alias for stop. Process snapshots include both camelCase and Hermes-style snake_case aliases such as session_id, task_id, backend, env_type, notify_on_complete, watch_patterns, exit_command, exit_code, stdout_tail, stderr_tail, conversation_id, and run_id. Use wait to block until a bounded job exits or times out; use log for paged stdout/stderr tail; use submit to write a line with newline. watchPatterns snapshots include watchStats/watch_stats with match/emit/drop counts, first/last match times, by-pattern/by-stream counters, and Hermes-style global flood counters globalSuppressedCount/globalTrippedCount. For bounded long tasks, prefer notifyOnComplete=true; for rare long-lived readiness signals, use watchPatterns. If neither is set, poll state/list/log or wait to avoid silent background jobs."#,
        ),
        (
            "execute_code",
            r#"- execute_code: payload {"language":"python|javascript|powershell","code":"print('ok')","cwd":".","taskId":"optional session","sessionId":"optional session","timeoutSeconds":60}. Local Python writes a short-lived workspace scratch file and exposes hermes_tools.py over loopback RPC so scripts can call web_search, web_extract, read_file, write_file, search_files, patch, and terminal. With TERMINAL_ENV=docker|ssh|singularity|modal|daytona, Python execute_code ships the script and hermes_tools.py to the selected backend and proxies those same tool calls through Hermes-style file RPC request/response files; non-Python remote languages run through the selected terminal backend using heredoc input so they share backend cwd/session, mounts, sync, timeout, and lifecycle behavior when that backend supports those features."#,
        ),
        (
            "workspace_diagnostics",
            r#"- workspace_diagnostics: payload {"mode":"auto|rust|typescript|python|go|all","workspaceDir":".","timeoutSeconds":90,"maxCommands":4} runs bounded diagnostics, or {"action":"status|list|lsp_status|lsp_list|which|install|install_all|start|stop|restart|clients|lsp_diagnostics|lsp_snapshot_baseline|lsp_clear_baseline","workspaceDir":".","server":"rust-analyzer","path":"src/main.rs","installedOnly":false,"delta":true,"execute":false} reports Hermes-style LSP server metadata, resolves binaries, dry-runs or explicitly executes LSP install recipes, manages persistent LSP server processes, initializes JSON-RPC clients, sends didOpen/didChange/didSave for one file to collect publishDiagnostics, tracks broken clients/idle reap, and supports Claude/Hermes-style diagnostic baseline snapshots so lsp_diagnostics can return only newly introduced diagnostics."#,
        ),
        (
            "env_probe",
            r#"- env_probe: payload {"commands":["optional command names"]} returns a read-only Hermes-style local environment probe: OS/arch, TERMINAL_ENV, workspace signals, Python/pip/uv state, and command availability."#,
        ),
        (
            "credential_pool",
            r#"- credential_pool: payload {"action":"status"} shows redacted LLM credential cooldown status; {"action":"reset","providerId":"optional provider id"} clears credential cooldowns; {"action":"files","containerBase":"/root/.synthchat"} lists configured credential-file mounts; {"action":"skills","containerBase":"/root/.synthchat","limit":100} lists skill directory mounts/files; {"action":"cache","containerBase":"/root/.synthchat","limit":100} lists artifact cache mounts/files; {"action":"sync_files","containerBase":"/root/.synthchat","limit":100} lists credential+skill+cache files for future remote sandbox sync; {"action":"translate_cache_path","hostPath":"path"} maps a host artifact cache path to the agent-visible sandbox path."#,
        ),
        (
            "osv_check",
            r#"- osv_check: payload {"package":"@scope/pkg","ecosystem":"npm|PyPI","version":"optional"} or {"command":"npx|uvx|pipx","args":["pkg@1.0.0"]}. Queries OSV and reports MAL-* malware advisories only."#,
        ),
        (
            "computer_use",
            r#"- computer_use: payload {"action":"status|capabilities|backend_status|requirements|setup_schema|session_status|mcp_session_status|reset_backend|mcp_probe|capture|click|double_click|right_click|middle_click|drag|scroll|type|key|set_value|wait|list_apps|focus_app","mode":"som|vision|ax","max_elements":100,"element":1,"from_element":1,"to_element":2,"coordinate":[x,y],"from_coordinate":[x,y],"to_coordinate":[x,y],"text":"text","value":"text for set_value","keys":"ctrl+s","seconds":1,"app":"optional app/title","capture_after":false,"timeoutSeconds":10}. Desktop automation; call status/capabilities/backend_status/requirements/setup_schema first when backend availability is uncertain; use mcp_session_status/session_status to inspect active persistent cua-driver MCP lifecycle without desktop actions; use mcp_probe to initialize a one-shot cua-driver mcp process and list MCP tools without performing desktop actions. Then prefer capture/list_apps/wait before mutating actions. reset_backend clears CUA MCP lifecycle diagnostics and stops the macOS persistent cua-driver MCP session when present. On Windows, capture mode=som returns a screenshot artifact with numbered UI Automation overlays plus the matching element list; element targets resolve to the last capture's element centers. Capture max_elements defaults to 100 and clamps to 1000, returning totalElements/truncatedElements when dense UIA trees are trimmed; pass app to scope capture to a matching process/title window. Use set_value with element or coordinate for editable/selectable UIA controls when typing would be less reliable; dangerous typed shell patterns and destructive system shortcuts are hard-blocked."#,
        ),
        (
            "delegate_task",
            r#"- delegate_task: payload {"task":"focused subtask","role":"researcher|planner|coder","toolsets":["file","browser"],"canDelegate":false}"#,
        ),
        (
            "mixture_of_agents",
            r#"- mixture_of_agents: payload {"user_prompt":"hard problem","referenceProviderIds":["optional provider ids"],"aggregatorProviderId":"optional","referenceCount":4,"minSuccessfulReferences":1}. Routes a hard problem through multiple LLM calls and synthesizes a final answer."#,
        ),
        (
            "kanban_create",
            r#"- kanban_create: payload {"title":"task title","body":"details","assignee":"optional","priority":0,"parents":["task-id"]} creates a local agent kanban task."#,
        ),
        (
            "kanban_list",
            r#"- kanban_list: payload {"status":"optional","assignee":"optional","limit":50,"includeArchived":false} lists local agent kanban tasks."#,
        ),
        (
            "kanban_show",
            r#"- kanban_show: payload {"taskId":"task id"} shows a kanban task with comments/events/links."#,
        ),
        (
            "kanban_complete",
            r#"- kanban_complete: payload {"taskId":"task id","summary":"what was completed","result":"optional","metadata":{"changed_files":["..."]},"created_cards":["task ids created during this run"],"artifacts":["absolute or workspace file paths"]} marks a kanban task completed. created_cards accepts a string or array and is validated so phantom task ids are rejected before completion; pass created_cards=[] to skip this check. artifacts accepts a string or array and is merged into metadata.artifacts for downstream handoff/attachments."#,
        ),
        (
            "kanban_block",
            r#"- kanban_block: payload {"taskId":"task id","reason":"why blocked"} marks a kanban task blocked."#,
        ),
        (
            "kanban_unblock",
            r#"- kanban_unblock: payload {"taskId":"task id","note":"optional"} moves a blocked kanban task back to ready."#,
        ),
        (
            "kanban_heartbeat",
            r#"- kanban_heartbeat: payload {"taskId":"task id","note":"progress note"} records task liveness/progress."#,
        ),
        (
            "kanban_comment",
            r#"- kanban_comment: payload {"taskId":"task id","body":"comment","author":"optional"} appends a kanban task comment."#,
        ),
        (
            "kanban_link",
            r#"- kanban_link: payload {"parentId":"parent task id","childId":"child task id"} links kanban task dependencies."#,
        ),
        (
            "send_message",
            r#"- send_message: payload {"action":"list"} returns local targets plus configured externalTargets and Hermes-style directoryTargets; payload {"action":"import_directory","directory":{"updated_at":"...","platforms":{"slack":[{"id":"C...","name":"engineering","type":"channel"}]}}} writes channel_directory.json; payload {"action":"refresh_directory","url":"https://...","token":"optional bearer","timeoutSeconds":15} fetches and writes channel_directory.json; payload {"action":"refresh_directory","platform":"mattermost"} builds channel_directory.json from the configured Mattermost teams/channels; payload {"target":"current|conversationId|title|discord|discord:<channel_id>|feishu:<receive_id>|feishu:<receive_id>:<reply_message_id>|telegram|telegram:<chat_id>|telegram:<chat_id>:<message_thread_id>|slack|slack:<channel_id>|slack:<channel_id>:<thread_ts>|slack:<user_id>|mattermost|mattermost:<channel_id>|mattermost:<channel_id>:<root_id>|matrix|matrix:<room_id>|signal|signal:<recipient>|signal:group:<group_id>|email|email:<address>|sms|sms:<phone>|dingtalk|dingtalk:<target>|whatsapp|whatsapp:<chat_id>|qqbot|qqbot:<id>|homeassistant|homeassistant:<notify_target>|bluebubbles|bluebubbles:<chat_id>|wecom:<chat_id>|weixin:<chat_id>|yuanbao:direct:<account_id>|yuanbao:group:<group_code>","message":"text to send","role":"assistant|user","platform":"optional discord|feishu|telegram|slack|mattermost|matrix|signal|email|sms|dingtalk|whatsapp|qqbot|homeassistant|bluebubbles|wecom|weixin|yuanbao","channel_id":"optional Discord/Telegram/Slack/Mattermost/QQBot channel id","chat_id":"optional Telegram/WhatsApp/QQBot/Home Assistant/BlueBubbles/WeCom/Weixin target","room_id":"optional Matrix room id","recipient":"optional Signal recipient","to":"optional Email/SMS target","subject":"optional Email subject","receive_id":"optional Feishu receive id","receive_id_type":"chat_id|open_id|union_id|email","user_id":"optional Yuanbao account id"} sends to local SynthChat conversations, Discord through configured bot/bridge, Feishu/Lark OpenAPI with MEDIA:<path> image/file uploads, Telegram Bot API with MEDIA:<path> photo/video/voice/audio/document uploads plus [[as_document]] force-document routing, Slack chat.postMessage text routing, Slack user IDs U... via conversations.open DM routing, Mattermost REST text posts and MEDIA:<path> local file uploads, Matrix Client-Server API routing with MEDIA:<path> uploads for unencrypted rooms, Signal signal-cli JSON-RPC with MEDIA:<path> attachments, Email SMTP text routing, SMS/Twilio text routing, DingTalk robot webhook text routing, WhatsApp bridge text routing, QQBot REST text routing, Home Assistant notify text routing, BlueBubbles iMessage text and MEDIA:<path> attachment routing, Yuanbao direct DM through the configured bridge, or WeCom/Weixin/Yuanbao group through settings.messagingGateway when configured. Bare platform targets require corresponding settings.* home target; named targets can resolve through Hermes channel_directory.json. In cron runs, duplicate sends to the configured HERMES_CRON_AUTO_DELIVER_* target are skipped because the final response will be auto-delivered there."#,
        ),
        (
            "session_search",
            r#"- session_search: payload {"query":"topic","limit":5,"kind":"all|message|run|tool|artifact"} or {"conversationId":"...","messageId":"...","window":5}"#,
        ),
        (
            "clarify",
            r#"- clarify: payload {"question":"one concise question","choices":["optional choice 1","optional choice 2"]}"#,
        ),
        (
            "cronjob",
            r#"- cronjob: payload {"action":"list|create|update|pause|resume|delete|trigger","jobId":"optional","name":"optional","prompt":"task to run","schedule":"30m|every 2h|0 9 * * *|RFC3339","scheduleKind":"once|interval|cron","runAt":"RFC3339","intervalMinutes":60,"cronExpr":"0 9 * * *","repeat":3,"profile":"persona id/name","personaId":"persona id","agentId":"agent id/name","skills":["skill/name"],"contextFrom":["job id/name"],"script":"relative path under data/scripts","noAgent":false,"provider":"optional","model":"optional","baseUrl":"optional provider endpoint override","timeoutSeconds":600,"scriptTimeoutSeconds":600,"workdir":"absolute directory","deliver":"origin|local|all|telegram|telegram:<chat_id>:<thread_id>|discord|discord:<channel_id>|slack|slack:<channel_id>","origin":{"platform":"synthchat","conversationId":"..."}} creates/updates scheduled work. timeoutSeconds overrides the cron agent inactivity timeout; scriptTimeoutSeconds overrides pre-run/noAgent script timeout; 0 means unlimited. Omit deliver to auto-deliver back to the creating SynthChat conversation; use local to save output only; use all or a bare platform name to deliver to configured home targets. [SILENT] final output suppresses delivery."#,
        ),
        (
            "recall_memory",
            r#"- recall_memory: payload {"query":"user preference or durable fact","limit":8}"#,
        ),
        (
            "remember_fact",
            r#"- remember_fact: payload {"summary":"stable user fact or preference","importance":1-5}"#,
        ),
        (
            "manage_memory",
            r#"- manage_memory: payload {"action":"read|add|replace|remove","query":"optional","id":"memory id","summary":"memory text","importance":1-5,"limit":8}"#,
        ),
        (
            "memory",
            r#"- memory: payload {"action":"search|read|add|replace|remove","query":"optional","summary":"stable memory","id":"memory id","importance":1-5,"limit":8}. Hermes-compatible alias for memory operations."#,
        ),
        (
            "skills_list",
            r#"- skills_list: payload {"query":"optional","enabledOnly":false}"#,
        ),
        (
            "skill_view",
            r#"- skill_view: payload {"name":"skill id or name","filePath":"optional relative file","maxChars":20000}"#,
        ),
        (
            "skill_manage",
            r#"- skill_manage: payload {"action":"create|edit|patch|delete|write_file|remove_file","name":"skill-name","content":"full SKILL.md","category":"optional","filePath":"references/file.md","fileContent":"...","oldString":"...","newString":"...","replaceAll":false}"#,
        ),
        (
            "image_generate",
            r#"- image_generate: payload {"prompt":"image prompt","size":"1024x1024","n":1}"#,
        ),
        (
            "video_generate",
            r#"- video_generate: payload {"prompt":"video prompt","operation":"generate|edit|extend","imageUrl":"optional image URL","videoUrl":"optional source video URL","duration":8,"aspectRatio":"16:9","resolution":"720p","negativePrompt":"optional","audio":false,"seed":123,"extra":{}}. Uses the enabled video provider."#,
        ),
        (
            "text_to_speech",
            r#"- text_to_speech: payload {"text":"speech text","voice":"alloy","model":"gpt-4o-mini-tts","format":"mp3","speed":1.0}"#,
        ),
        (
            "transcribe_audio",
            r#"- transcribe_audio: payload {"path":"relative audio path"} or {"url":"https://example.com/voice.mp3","model":"whisper-1","language":"zh"}"#,
        ),
        (
            "vision_analyze",
            r#"- vision_analyze: payload {"prompt":"what to inspect","path":"relative image path"} or {"prompt":"...","url":"https://example.com/image.png"}"#,
        ),
        (
            "video_analyze",
            r#"- video_analyze: payload {"videoUrl":"https://example.com/video.mp4","question":"what happens in this video?","model":"optional"}"#,
        ),
        (
            "weather",
            r#"- weather: payload {"location":"city or place","lang":"zh|en","unit":"m|i","includeForecast":true,"days":3}. Uses configured QWeather settings."#,
        ),
        (
            "ha_list_entities",
            r#"- ha_list_entities: payload {"domain":"optional light|sensor|switch","area":"optional area text","limit":100}. Lists Home Assistant entities."#,
        ),
        (
            "ha_get_state",
            r#"- ha_get_state: payload {"entityId":"light.living_room"} gets one Home Assistant entity state."#,
        ),
        (
            "ha_list_services",
            r#"- ha_list_services: payload {"domain":"optional light|climate"} lists Home Assistant service actions."#,
        ),
        (
            "ha_call_service",
            r#"- ha_call_service: payload {"domain":"light","service":"turn_on","entityId":"light.living_room","data":{"brightness":128}} calls a Home Assistant service."#,
        ),
        (
            "feishu_doc_read",
            r#"- feishu_doc_read: payload {"doc_token":"document token"} reads Feishu/Lark docx raw content."#,
        ),
        (
            "feishu_drive_list_comments",
            r#"- feishu_drive_list_comments: payload {"file_token":"doc file token","file_type":"docx","is_whole":false,"page_size":100,"page_token":"optional"} lists Feishu/Lark document comments."#,
        ),
        (
            "feishu_drive_list_comment_replies",
            r#"- feishu_drive_list_comment_replies: payload {"file_token":"doc file token","comment_id":"comment id","file_type":"docx","page_size":100,"page_token":"optional"} lists Feishu/Lark comment replies."#,
        ),
        (
            "feishu_drive_reply_comment",
            r#"- feishu_drive_reply_comment: payload {"file_token":"doc file token","comment_id":"comment id","content":"plain text","file_type":"docx"} replies to a Feishu/Lark document comment."#,
        ),
        (
            "feishu_drive_add_comment",
            r#"- feishu_drive_add_comment: payload {"file_token":"doc file token","content":"plain text","file_type":"docx"} adds a whole-document Feishu/Lark comment."#,
        ),
        (
            "yb_query_group_info",
            r#"- yb_query_group_info: payload {"group_code":"yuanbao group code"} queries Yuanbao group/Pai info via configured Yuanbao bridge."#,
        ),
        (
            "yb_query_group_members",
            r#"- yb_query_group_members: payload {"group_code":"yuanbao group code","action":"find|list_bots|list_all","name":"optional","mention":false} queries Yuanbao group members."#,
        ),
        (
            "yb_send_dm",
            r#"- yb_send_dm: payload {"group_code":"source group","name":"target nickname","message":"text","user_id":"optional","media_files":[{"path":"absolute file","is_voice":false}]} sends Yuanbao DM via bridge."#,
        ),
        (
            "yb_search_sticker",
            r#"- yb_search_sticker: payload {"query":"贴纸关键词","limit":10} searches configured Yuanbao sticker catalogue or bridge."#,
        ),
        (
            "yb_send_sticker",
            r#"- yb_send_sticker: payload {"sticker":"name or id","chat_id":"direct:...|group:...","reply_to":"optional"} sends Yuanbao sticker via bridge."#,
        ),
        (
            "spotify_playback",
            r#"- spotify_playback: payload {"action":"get_state|get_currently_playing|play|pause|next|previous|seek|set_repeat|set_shuffle|set_volume|recently_played","device_id":"optional","market":"US","context_uri":"spotify:album|playlist|artist:...","uris":["spotify:track:..."],"offset":{},"position_ms":0,"state":"track|context|off|true|false","volume_percent":50,"limit":20,"after":0,"before":0}. Controls or reads Spotify playback."#,
        ),
        (
            "spotify_devices",
            r#"- spotify_devices: payload {"action":"list|transfer","device_id":"spotify connect device id","play":false}. Lists Spotify Connect devices or transfers playback."#,
        ),
        (
            "spotify_queue",
            r#"- spotify_queue: payload {"action":"get|add","uri":"spotify uri/id/url","device_id":"optional"}. Reads Spotify queue or adds an item."#,
        ),
        (
            "spotify_search",
            r#"- spotify_search: payload {"query":"search text","types":["track","album","artist","playlist"],"limit":10,"offset":0,"market":"US","include_external":"audio"}. Searches Spotify catalog."#,
        ),
        (
            "spotify_playlists",
            r#"- spotify_playlists: payload {"action":"list|get|create|add_items|remove_items|update_details","playlist_id":"id/uri/url","name":"playlist name","description":"optional","public":false,"collaborative":false,"uris":["spotify:track:..."],"position":0,"snapshot_id":"optional","limit":20,"offset":0,"market":"US"}. Manages Spotify playlists."#,
        ),
        (
            "spotify_albums",
            r#"- spotify_albums: payload {"action":"get|tracks","album_id":"id/uri/url","id":"alias","market":"US","limit":20,"offset":0}. Reads Spotify album metadata or tracks."#,
        ),
        (
            "spotify_library",
            r#"- spotify_library: payload {"kind":"tracks|albums","action":"list|save|remove","limit":20,"offset":0,"market":"US","uris":["spotify:track|album:..."],"ids":["id"],"items":["id/uri/url"]}. Reads or edits saved Spotify tracks/albums."#,
        ),
        (
            "discord",
            r#"- discord: payload {"action":"fetch_messages|search_members|create_thread|send_message","channel_id":"channel id","guild_id":"server id","query":"member prefix","name":"thread name","content":"message text","message_id":"optional anchor/reply","limit":50,"before":"snowflake","after":"snowflake","auto_archive_duration":1440}. Reads and participates in Discord via bot token or configured bridge."#,
        ),
        (
            "discord_admin",
            r#"- discord_admin: payload {"action":"list_guilds|server_info|list_channels|channel_info|list_roles|member_info|list_pins|pin_message|unpin_message|delete_message|add_role|remove_role","guild_id":"server id","channel_id":"channel id","user_id":"user id","role_id":"role id","message_id":"message id","limit":50}. Discord server administration via bot token or bridge."#,
        ),
        (
            "todo",
            r#"- todo: Manage the current run's task list. Call with {} to read. Write with payload {"todos":[{"id":"inspect","content":"inspect code","status":"in_progress"}],"merge":false}; merge=false replaces the list, merge=true updates existing items by id and appends new ones. Status values: pending|in_progress|completed|cancelled; keep at most one item in_progress; mark items completed immediately when done. Returns the full list and summary counts."#,
        ),
        (
            "update_todo",
            "- update_todo: alias for todo with the same read/write/merge behavior and statuses pending|in_progress|completed|cancelled.",
        ),
        (
            "checkpoint",
            r#"- checkpoint: payload {"summary":"what is done","state":"after_inspection","completedCallIds":[],"eventRefs":[]}"#,
        ),
        (
            "artifact",
            r#"- artifact: payload {"name":"notes","content":"text to save"} or {"action":"publish_file","path":"workspace file","name":"optional"} publishes an existing workspace file as a clickable artifact."#,
        ),
        ("list_artifacts", r#"- list_artifacts: payload {}"#),
        (
            "browser_navigate",
            r#"- browser_navigate: payload {"url":"https://example.com"}"#,
        ),
        (
            "browser_snapshot",
            r#"- browser_snapshot: payload {"url":"https://example.com","full":false}"#,
        ),
        ("browser_back", r#"- browser_back: payload {}"#),
        (
            "browser_get_images",
            r#"- browser_get_images: payload {"url":"https://example.com"}"#,
        ),
        (
            "browser_provider",
            r#"- browser_provider: payload {"action":"status|list|resolve|setup_schema|lifecycle|health_schema","provider":"optional provider id/name/type"} inspects configured cloud browser providers, Hermes-style active provider resolution, credential presence, setup schema, and non-mutating create/close lifecycle diagnostics without creating a session."#,
        ),
        (
            "browser_create_session",
            r#"- browser_create_session: payload {"taskId":"optional"} returns sessionId and cdpUrl for dynamic browser work."#,
        ),
        (
            "browser_close_session",
            r#"- browser_close_session: payload {"sessionId":"..."}"#,
        ),
        (
            "browser_cdp",
            r#"- browser_cdp: payload {"cdpUrl":"ws://127.0.0.1:9222/devtools/page/...","action":"snapshot|navigate|click|type|press|scroll|back|screenshot|console|dialog|frame_tree|evaluate|raw","maxItems":60}; screenshot saves a persistent artifact and returns screenshotPath. Raw CDP payload {"cdpUrl":"ws://...","method":"Runtime.evaluate","params":{"expression":"document.title"},"timeoutMs":10000,"targetId":"optional","sessionId":"optional","frameId":"optional supervisor frame id"}"#,
        ),
        (
            "browser_click",
            r#"- browser_click: payload {"cdpUrl":"ws://...","ref":"@e5"} or {"selector":"button[type=submit]"}"#,
        ),
        (
            "browser_type",
            r#"- browser_type: payload {"cdpUrl":"ws://...","ref":"@e3","text":"hello","clear":true} or {"selector":"input[name=q]","text":"hello"}"#,
        ),
        (
            "browser_press",
            r#"- browser_press: payload {"cdpUrl":"ws://...","key":"Enter"}"#,
        ),
        (
            "browser_scroll",
            r#"- browser_scroll: payload {"cdpUrl":"ws://...","x":0,"y":700}"#,
        ),
        (
            "browser_dialog",
            r#"- browser_dialog: respond to a pending JS dialog observed by browser_snapshot/browser_supervisor_state. payload {"action":"accept|dismiss","dialogId":"optional","promptText":"optional"}; cdpUrl is optional when a supervisor is active."#,
        ),
        (
            "browser_record",
            r#"- browser_record: CDP screencast recording. payload {"action":"start|stop|status|export|capabilities","cdpUrl":"optional ws://...","runId":"optional","everyNthFrame":1,"quality":80,"maxFrames":12,"format":"auto|webm|png","fps":4}. start records Page.screencastFrame data into supervisor state; export saves recent PNG frame artifacts and a JSON manifest with network/console evidence, and when ffmpeg is available also assembles a WebM video artifact."#,
        ),
        (
            "browser_vision",
            r#"- browser_vision: payload {"cdpUrl":"ws://...","question":"what to inspect visually","fullPage":false}"#,
        ),
        (
            "browser_console",
            r#"- browser_console: payload {"cdpUrl":"ws://...","expression":"document.title"}"#,
        ),
        (
            "browser_supervisor_register",
            r#"- browser_supervisor_register: payload {"cdpUrl":"ws://...","sessionId":"optional","providerType":"cdp","dialogPolicy":"must_respond|auto_dismiss|auto_accept","dialogTimeoutSeconds":300} attaches a Hermes-style CDP supervisor for dialogs, frames, console, network, and screencast state."#,
        ),
        (
            "browser_supervisor_state",
            r#"- browser_supervisor_state: payload {"runId":"optional"} returns raw state, summary, and supervisor capabilities including Hermes-style dialog policy metadata."#,
        ),
        (
            "browser_supervisor_remove",
            r#"- browser_supervisor_remove: payload {"sessionId":"..."}"#,
        ),
        (
            "web_provider",
            r#"- web_provider: payload {"action":"status|list|resolve|setup_schema|lifecycle|health_schema","capability":"search|extract","provider":"optional provider id/name/type"} inspects configured web search providers, Hermes-style capability-aware provider resolution, and pending provider adapter parity without network calls."#,
        ),
        (
            "web_search",
            r#"- web_search: payload {"query":"search terms","limit":5,"language":"optional"}"#,
        ),
        (
            "x_search",
            r#"- x_search: payload {"query":"topic on X/Twitter","limit":5,"from":"optional username","since":"YYYY-MM-DD","until":"YYYY-MM-DD"}. Uses configured web_search provider as a compatibility bridge."#,
        ),
        (
            "web_extract",
            r#"- web_extract: payload {"url":"https://example.com/page","maxChars":6000} or {"urls":["https://example.com/a","https://example.com/b"]}"#,
        ),
        (
            "web_request",
            r#"- web_request: payload {"url":"https://example.com/api","method":"GET","headers":{},"body":null}"#,
        ),
    ]
}

pub(super) fn available_mcp_tool_definitions(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<Vec<ToolDefinition>> {
    if !agent.mcp_enabled {
        return Ok(vec![]);
    }
    let mut tools = store
        .tool_definitions()?
        .into_iter()
        .filter(|tool| {
            agent.enabled_mcp_servers.is_empty()
                || agent.enabled_mcp_servers.contains(&tool.server_id)
        })
        .collect::<Vec<_>>();
    tools.extend(
        python_plugin_tool_definitions(store)?
            .into_iter()
            .filter(|tool| {
                agent.enabled_mcp_servers.is_empty()
                    || agent.enabled_mcp_servers.contains(&tool.server_id)
            }),
    );
    tools.retain(|tool| tool_allowed_by_agent_capabilities(tool, agent));
    tools = apply_agent_toolset_policy(tools, agent);
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools.truncate(40);
    Ok(tools)
}

pub(super) fn visible_tool_definitions_for_agent(
    store: &AppStore,
    agent: &AgentDefinition,
    context: ToolExecutionContext,
) -> AppResult<Vec<ToolDefinition>> {
    let availability = internal_tool_availability(store);
    let mut tools = internal_tool_prompt_lines()
        .into_iter()
        .filter(|(name, _)| internal_tool_available(name, &availability))
        .map(|(name, line)| ToolDefinition {
            name: name.into(),
            display_name: name.into(),
            description: internal_tool_prompt_line_for_agent(name, line, agent)
                .trim_start_matches("- ")
                .to_string(),
            source: "internal".into(),
            server_id: "__internal".into(),
            tool_name: name.into(),
            input_schema: json!({}),
            requires_approval: false,
        })
        .collect::<Vec<_>>();
    if agent.mcp_enabled {
        tools.extend(store.tool_definitions()?.into_iter().filter(|tool| {
            agent.enabled_mcp_servers.is_empty()
                || agent.enabled_mcp_servers.contains(&tool.server_id)
        }));
        tools.extend(
            python_plugin_tool_definitions(store)?
                .into_iter()
                .filter(|tool| {
                    agent.enabled_mcp_servers.is_empty()
                        || agent.enabled_mcp_servers.contains(&tool.server_id)
                }),
        );
    }
    tools = apply_agent_toolset_policy(tools, agent);
    tools = apply_tool_context_policy(tools, context);
    tools.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.server_id.cmp(&right.server_id))
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    Ok(tools)
}

fn python_plugin_tool_definitions(store: &AppStore) -> AppResult<Vec<ToolDefinition>> {
    let mut tools = Vec::new();
    let mut seen = HashSet::new();
    for tool in list_python_plugin_tools(store)? {
        let server_id = format!("{PYTHON_PLUGIN_SERVER_PREFIX}{}", tool.plugin_id);
        if !seen.insert((server_id.clone(), tool.name.clone())) {
            continue;
        }
        let description = if tool.description.trim().is_empty() {
            format!(
                "Python plugin tool registered by {} ({})",
                tool.plugin_name, tool.toolset
            )
        } else {
            tool.description
        };
        tools.push(ToolDefinition {
            name: tool.name.clone(),
            display_name: tool.name.clone(),
            description,
            source: "python-plugin".into(),
            server_id,
            tool_name: tool.name,
            input_schema: if tool.schema.is_object() {
                tool.schema
            } else {
                json!({})
            },
            requires_approval: false,
        });
    }
    for plugin in store
        .plugins()?
        .into_iter()
        .filter(|plugin| plugin.enabled)
        .filter(|plugin| !matches!(plugin.kind.as_str(), "exclusive" | "model-provider"))
        .filter(|plugin| {
            plugin
                .requires_env
                .iter()
                .all(|name| name.trim().is_empty() || env::var_os(name).is_some())
        })
    {
        let server_id = format!("{PYTHON_PLUGIN_SERVER_PREFIX}{}", plugin.id);
        for tool_name in plugin.provided_tools {
            if !seen.insert((server_id.clone(), tool_name.clone())) {
                continue;
            }
            let description = if plugin.description.trim().is_empty() {
                format!("Python plugin tool registered by {}", server_id)
            } else {
                plugin.description.clone()
            };
            tools.push(ToolDefinition {
                name: tool_name.clone(),
                display_name: tool_name.clone(),
                description,
                source: "python-plugin".into(),
                server_id: server_id.clone(),
                tool_name,
                input_schema: json!({"type": "object", "additionalProperties": true}),
                requires_approval: false,
            });
        }
    }
    Ok(tools)
}

pub(super) fn render_mcp_tool_definitions(tools: &[ToolDefinition]) -> String {
    if tools.is_empty() {
        return "No MCP or capability tools are currently registered.".into();
    }
    tools
        .iter()
        .map(|tool| {
            let schema = serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".into());
            let schema = truncate_for_prompt(&schema, 600);
            let hermes_alias = mcp_tool_alias_name(tool);
            let alias_suffix = if hermes_alias == tool.name {
                String::new()
            } else {
                format!(" aliases=[{hermes_alias}]")
            };
            format!(
                "- {}{}: {} payloadSchema={}{}",
                tool.name,
                alias_suffix,
                tool.description.trim(),
                schema,
                if tool.requires_approval {
                    " requiresApproval=true"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn truncate_for_prompt(value: &str, max_chars: usize) -> String {
    let redacted = redact_sensitive_text(value);
    if redacted.chars().count() <= max_chars {
        return redacted;
    }
    let mut truncated = redacted.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

pub(super) fn resolve_mcp_tool(
    tools: &[ToolDefinition],
    requested: &str,
) -> Option<ToolDefinition> {
    let requested = requested.trim();
    tools
        .iter()
        .find(|tool| mcp_tool_request_matches(tool, requested))
        .cloned()
}

fn mcp_tool_request_matches(tool: &ToolDefinition, requested: &str) -> bool {
    tool.name == requested
        || tool.display_name == requested
        || tool.tool_name == requested
        || format!("{}.{}", tool.server_id, tool.tool_name) == requested
        || mcp_tool_alias_name(tool) == requested
}

fn mcp_tool_alias_name(tool: &ToolDefinition) -> String {
    if tool.source == "mcp_utility" {
        tool.name.clone()
    } else {
        hermes_mcp_tool_name(&tool.server_id, &tool.tool_name)
    }
}

fn hermes_mcp_tool_name(server_id: &str, tool_name: &str) -> String {
    format!(
        "mcp_{}_{}",
        sanitize_mcp_name_component(server_id),
        sanitize_mcp_name_component(tool_name)
    )
}

fn sanitize_mcp_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) async fn execute_recovery_mcp_tool(
    store: &AppStore,
    run_id: &str,
    definition: &ToolDefinition,
    payload: Value,
) -> AppResult<(String, ToolEvent)> {
    let replay_payload = payload.clone();
    let payload = strip_provider_tool_call_metadata(payload);
    run_pre_tool_call_hooks(store, run_id, &definition.tool_name, &payload).await?;
    if definition
        .server_id
        .starts_with(PYTHON_PLUGIN_SERVER_PREFIX)
    {
        let started = Instant::now();
        let result = run_python_plugin_tool(store, &definition.tool_name, &payload).await;
        let elapsed_ms = started.elapsed().as_millis();
        let (ok, mut text, error) = match result {
            Ok(text) => (true, redact_sensitive_text(&text), None),
            Err(error) => (
                false,
                String::new(),
                Some(redact_sensitive_text(&error.to_string())),
            ),
        };
        text = run_transform_tool_result_hooks(
            store,
            run_id,
            &definition.tool_name,
            &payload,
            &text,
            ok,
            error.as_deref(),
        )
        .await;
        let event = ToolEvent {
            status: Some(if ok { "completed" } else { "failed" }.into()),
            reference_id: None,
            call_id: Some(provider_tool_call_id(&replay_payload).unwrap_or_else(|| new_id("call"))),
            run_id: Some(run_id.to_string()),
            checkpoint_id: None,
            event_type: "python_plugin_tool".into(),
            server_id: definition.server_id.clone(),
            tool_name: definition.tool_name.clone(),
            ok,
            timed_out: false,
            elapsed_ms,
            kind: tool_event_kind(&definition.server_id, &definition.tool_name, None),
            title: format!("python-plugin · {}", definition.tool_name),
            summary: if ok {
                summarize_tool_text(&text)
            } else {
                error
                    .clone()
                    .unwrap_or_else(|| "python plugin tool failed".into())
            },
            path: None,
            exists: None,
            mime_type: Some("text/plain".into()),
            text: if text.is_empty() {
                None
            } else {
                Some(text.clone())
            },
            error: error.clone(),
            raw: Some(redact_json_value(
                json!({"payload": replay_payload.clone()}),
            )),
        };
        let hook_result = json!({
            "ok": ok,
            "text": text.clone(),
            "error": error.clone(),
            "event": event.clone(),
        });
        let _ =
            run_post_tool_call_hooks(store, run_id, &definition.tool_name, &payload, &hook_result)
                .await;
        if let Some(error) = error {
            return Err(AppError::BadRequest(error));
        }
        return Ok((text, event));
    }
    let result = call_mcp_tool_with_retry(
        store,
        definition.server_id.clone(),
        definition.tool_name.clone(),
        payload.clone(),
        None,
        store.config()?.chat.tool_call_retry_count,
        store.config()?.chat.tool_call_retry_backoff_ms,
    )
    .await?;
    let mut event = mcp_result_to_tool_event(run_id, definition, &result);
    event.call_id = Some(provider_tool_call_id(&replay_payload).unwrap_or_else(|| new_id("call")));
    event.raw = Some(redact_json_value(
        json!({"payload": replay_payload, "result": result}),
    ));
    let mut text = redact_sensitive_text(&mcp_result_text(&result));
    text = run_transform_tool_result_hooks(
        store,
        run_id,
        &definition.tool_name,
        &payload,
        &text,
        result.ok,
        result.error.as_deref(),
    )
    .await;
    event.text = if text.is_empty() {
        None
    } else {
        Some(text.clone())
    };
    if result.ok {
        event.summary = summarize_tool_text(&text);
    }
    let hook_result = json!({
        "ok": result.ok,
        "text": text.clone(),
        "error": result.error.clone(),
        "event": event.clone(),
    });
    let _ = run_post_tool_call_hooks(store, run_id, &definition.tool_name, &payload, &hook_result)
        .await;
    if text.trim().is_empty() && !result.ok {
        Ok((
            event
                .error
                .clone()
                .unwrap_or_else(|| "MCP tool call failed".into()),
            event,
        ))
    } else {
        Ok((text, event))
    }
}

fn strip_provider_tool_call_metadata(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.remove(PROVIDER_TOOL_CALL_META_KEY);
    }
    payload
}

pub(super) fn mcp_result_to_tool_event(
    run_id: &str,
    definition: &ToolDefinition,
    result: &McpCallResult,
) -> ToolEvent {
    let text = redact_sensitive_text(&mcp_result_text(result));
    let error = result.error.as_deref().map(redact_sensitive_text);
    ToolEvent {
        status: Some(if result.ok { "completed" } else { "failed" }.into()),
        reference_id: None,
        call_id: Some(new_id("call")),
        run_id: Some(run_id.to_string()),
        checkpoint_id: None,
        event_type: "mcp_tool".into(),
        server_id: definition.server_id.clone(),
        tool_name: definition.tool_name.clone(),
        ok: result.ok,
        timed_out: result.timed_out,
        elapsed_ms: result.elapsed_ms,
        kind: tool_event_kind(&definition.server_id, &definition.tool_name, None),
        title: format!("{} · {}", definition.server_id, definition.tool_name),
        summary: if result.ok {
            summarize_tool_text(&text)
        } else {
            error
                .clone()
                .unwrap_or_else(|| "MCP tool call failed".into())
        },
        path: None,
        exists: None,
        mime_type: Some("text/plain".into()),
        text: if text.is_empty() { None } else { Some(text) },
        error,
        raw: Some(redact_json_value(json!({"result": result}))),
    }
}

fn mcp_result_text(result: &McpCallResult) -> String {
    if !result.stdout.trim().is_empty() {
        result.stdout.clone()
    } else {
        result.stderr.clone()
    }
}

pub(super) fn tool_search_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
    context: ToolExecutionContext,
) -> AppResult<String> {
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 30) as usize;
    let include_unavailable = payload_bool(payload, &["includeUnavailable", "include_unavailable"]);
    let mut matches = tool_catalog(store, agent, context, include_unavailable)?
        .into_iter()
        .map(|entry| {
            let score = tool_catalog_relevance(&entry, query);
            (score, entry)
        })
        .filter(|(score, _)| query.is_empty() || *score > 0)
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| {
            left.1["name"]
                .as_str()
                .unwrap_or("")
                .cmp(right.1["name"].as_str().unwrap_or(""))
        })
    });
    let matches = matches
        .into_iter()
        .take(limit)
        .map(|(score, mut entry)| {
            entry["score"] = json!(score);
            entry
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&json!({
        "query": query,
        "includeUnavailable": include_unavailable,
        "count": matches.len(),
        "matches": matches
    }))?)
}

pub(super) fn tool_describe_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
    context: ToolExecutionContext,
) -> AppResult<String> {
    let requested = payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("tool_describe requires payload.name".into()))?;
    let include_unavailable = payload_bool(payload, &["includeUnavailable", "include_unavailable"]);
    let catalog = tool_catalog(store, agent, context, include_unavailable)?;
    let entry = catalog
        .iter()
        .find(|entry| tool_catalog_name_matches(entry, requested))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!("tool not found: {requested}")))?;
    Ok(serde_json::to_string_pretty(&entry)?)
}

pub(super) fn resolve_tool_call_payload(payload: &Value) -> AppResult<(String, Value)> {
    let name = payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("tool_call requires payload.name".into()))?;
    if matches!(name, "tool_search" | "tool_describe" | "tool_call") {
        return Err(AppError::BadRequest(format!(
            "tool_call cannot invoke bridge tool '{name}'"
        )));
    }
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("args"))
        .or_else(|| payload.get("payload"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = if let Some(raw) = arguments.as_str() {
        parse_tool_arguments_json(raw, name)
    } else {
        arguments
    };
    if !arguments.is_object() {
        return Err(AppError::BadRequest(
            "tool_call arguments must be a JSON object".into(),
        ));
    }
    Ok((name.to_string(), arguments))
}

fn tool_catalog(
    store: &AppStore,
    agent: &AgentDefinition,
    context: ToolExecutionContext,
    include_unavailable: bool,
) -> AppResult<Vec<Value>> {
    let mut entries = Vec::new();
    let availability = internal_tool_availability(store);
    for (name, line) in internal_tool_prompt_lines() {
        let tool = ToolDefinition {
            name: name.into(),
            display_name: name.into(),
            description: internal_tool_prompt_line_for_agent(name, line, agent)
                .trim_start_matches("- ")
                .to_string(),
            source: "internal".into(),
            server_id: "__internal".into(),
            tool_name: name.into(),
            input_schema: json!({}),
            requires_approval: false,
        };
        if !tool_allowed_in_context(&tool, context)
            || !tool_allowed_by_agent_capabilities(&tool, agent)
            || !tool_allowed_by_agent_toolsets(&tool, agent)
        {
            continue;
        }
        let available = internal_tool_available(name, &availability);
        if available || include_unavailable {
            let rendered_line = internal_tool_prompt_line_for_agent(name, line, agent);
            let unavailable_reason = if available {
                Value::Null
            } else {
                json!(internal_tool_unavailable_reason(name))
            };
            entries.push(json!({
                "name": name,
                "displayName": name,
                "source": "internal",
                "serverId": "__internal",
                "toolName": name,
                "description": rendered_line,
                "payloadShape": rendered_line,
                "requiresApproval": is_risky_tool_call(name, &json!({})),
                "available": available,
                "unavailableReason": unavailable_reason
            }));
        }
    }
    for tool in available_mcp_tool_definitions(store, agent)? {
        if tool_allowed_in_context(&tool, context) {
            let alias = mcp_tool_alias_name(&tool);
            entries.push(json!({
                "name": tool.name,
                "displayName": tool.display_name,
                "aliases": if alias == tool.name { json!([]) } else { json!([alias]) },
                "source": tool.source,
                "serverId": tool.server_id,
                "toolName": tool.tool_name,
                "description": tool.description,
                "payloadSchema": tool.input_schema,
                "requiresApproval": tool.requires_approval,
                "available": true,
                "unavailableReason": null
            }));
        }
    }
    Ok(entries)
}

fn internal_tool_prompt_line_for_agent(
    name: &str,
    line: &'static str,
    agent: &AgentDefinition,
) -> String {
    if name == "delegate_task" {
        return delegate_task_prompt_line(agent);
    }
    line.to_string()
}

fn delegate_task_prompt_line(agent: &AgentDefinition) -> String {
    let max_subagents = agent.max_subagents.max(1);
    let max_depth = agent.max_subagent_depth.max(1);
    let nested = if max_depth > 1 {
        format!(
            "Nested delegation is enabled up to maxSubagentDepth={max_depth}; child agents may delegate only when payload.canDelegate=true and depth remains below the limit."
        )
    } else {
        "Nested delegation is off for this agent; children are leaf workers and cannot call delegate_task."
            .into()
    };
    format!(
        r#"- delegate_task: single payload {{"task":"focused subtask","role":"researcher|planner|coder|orchestrator","toolsets":["file","browser"],"canDelegate":false}} or concurrent batch payload {{"tasks":[{{"goal":"subtask A","context":"needed details","toolsets":["file"],"role":"planner"}}]}}. Batch accepts up to maxSubagents={max_subagents} minus existing child runs. Current limits: maxSubagents={max_subagents}, maxSubagentDepth={max_depth}. {nested}"#
    )
}

fn internal_tool_unavailable_reason(tool_name: &str) -> &'static str {
    match tool_name {
        "browser_create_session" | "browser_close_session" => {
            "browser session provider is not configured or lacks credentials"
        }
        "web_search" | "x_search" => "search provider is not configured or enabled",
        "image_generate" => "image provider is not configured or enabled",
        "video_generate" => "video provider is not configured or enabled",
        "vision_analyze" | "video_analyze" | "browser_vision" => {
            "vision provider is not configured or enabled"
        }
        "text_to_speech" | "transcribe_audio" => "audio-capable LLM provider is not configured",
        "weather" => "QWeather settings are incomplete",
        "ha_list_entities" | "ha_get_state" | "ha_list_services" | "ha_call_service" => {
            "Home Assistant settings are incomplete"
        }
        "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment" => "Feishu/Lark settings are incomplete",
        "yb_query_group_info" | "yb_query_group_members" | "yb_send_dm" | "yb_send_sticker" => {
            "Yuanbao bridge is not configured"
        }
        "yb_search_sticker" => "Yuanbao bridge or sticker catalog is not configured",
        "spotify_playback" | "spotify_devices" | "spotify_queue" | "spotify_search"
        | "spotify_playlists" | "spotify_albums" | "spotify_library" => {
            "Spotify settings are incomplete"
        }
        "discord" | "discord_admin" => "Discord settings are incomplete",
        _ => "tool availability check failed",
    }
}

fn payload_bool(payload: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_catalog_relevance(entry: &Value, query: &str) -> usize {
    if query.trim().is_empty() {
        return 1;
    }
    let haystack = vec![
        entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("serverId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("aliases")
            .and_then(Value::as_array)
            .map(|aliases| {
                aliases
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        entry
            .get("payloadShape")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    ]
    .join(" ")
    .to_lowercase();
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| {
            let term = term.to_lowercase();
            if haystack.contains(&term) {
                if entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&term)
                {
                    4
                } else {
                    1
                }
            } else {
                0
            }
        })
        .sum()
}

pub(super) fn credential_pool_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "list" => {
            let status = store.llm_credential_pool_status()?;
            Ok(serde_json::to_string_pretty(&status)?)
        }
        "files" | "mounts" | "credential_files" => {
            let container_base = payload
                .get("containerBase")
                .or_else(|| payload.get("container_base"))
                .and_then(Value::as_str)
                .unwrap_or("/root/.synthchat");
            let mounts = store.credential_file_mounts(container_base)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "files",
                "mounts": mounts
            }))?)
        }
        "cache" | "cache_mounts" | "cache_files" => {
            let container_base = payload
                .get("containerBase")
                .or_else(|| payload.get("container_base"))
                .and_then(Value::as_str)
                .unwrap_or("/root/.synthchat");
            let file_limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(1000) as usize;
            let mounts = store.cache_directory_mounts(container_base, file_limit)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "cache",
                "mounts": mounts
            }))?)
        }
        "skills" | "skill_mounts" | "skill_files" => {
            let container_base = payload
                .get("containerBase")
                .or_else(|| payload.get("container_base"))
                .and_then(Value::as_str)
                .unwrap_or("/root/.synthchat");
            let file_limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(1000) as usize;
            let mounts = store.skills_directory_mounts(container_base, file_limit)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "skills",
                "mounts": mounts
            }))?)
        }
        "sync_files" | "sync-files" | "sync" => {
            let container_base = payload
                .get("containerBase")
                .or_else(|| payload.get("container_base"))
                .and_then(Value::as_str)
                .unwrap_or("/root/.synthchat");
            let file_limit = payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(1000) as usize;
            let files = store.remote_sync_files(container_base, file_limit)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "sync_files",
                "sync": files
            }))?)
        }
        "translate_cache_path" | "agent_visible_cache_path" | "cache_path" => {
            let container_base = payload
                .get("containerBase")
                .or_else(|| payload.get("container_base"))
                .and_then(Value::as_str)
                .unwrap_or("/root/.synthchat");
            let host_path = payload
                .get("hostPath")
                .or_else(|| payload.get("host_path"))
                .or_else(|| payload.get("path"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "credential_pool translate_cache_path requires hostPath".into(),
                    )
                })?;
            let path = store.to_agent_visible_cache_path(host_path, container_base)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "translate_cache_path",
                "path": path
            }))?)
        }
        "reset" | "clear" => {
            let provider_id = payload
                .get("providerId")
                .or_else(|| payload.get("provider_id"))
                .and_then(Value::as_str);
            let removed = store.reset_llm_credential_cooldowns(provider_id)?;
            let status = store.llm_credential_pool_status()?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "reset",
                "providerId": provider_id,
                "removedCooldowns": removed,
                "status": status,
            }))?)
        }
        other => Err(AppError::BadRequest(format!(
            "credential_pool action is not supported: {other}"
        ))),
    }
}

fn tool_catalog_name_matches(entry: &Value, requested: &str) -> bool {
    let requested = requested.trim();
    [
        entry.get("name").and_then(Value::as_str),
        entry.get("displayName").and_then(Value::as_str),
        entry.get("toolName").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .any(|candidate| candidate == requested)
        || entry
            .get("aliases")
            .and_then(Value::as_array)
            .is_some_and(|aliases| {
                aliases
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|candidate| candidate == requested)
            })
        || entry
            .get("serverId")
            .and_then(Value::as_str)
            .zip(entry.get("toolName").and_then(Value::as_str))
            .map(|(server_id, tool_name)| format!("{server_id}.{tool_name}") == requested)
            .unwrap_or(false)
}
