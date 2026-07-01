use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
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
    tool_toolsets, yuanbao_bridge_available, yuanbao_stickers_available, PythonPluginBridgeContext,
    ToolExecutionContext,
};

const PYTHON_PLUGIN_SERVER_PREFIX: &str = "__python_plugin:";
const TOOL_SEARCH_CHARS_PER_TOKEN: f64 = 4.0;

pub(super) fn render_internal_tool_prompt_block(
    agent: &AgentDefinition,
    context: ToolExecutionContext,
    availability: &InternalToolAvailability,
    store: Option<&AppStore>,
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
        .map(|(name, line)| internal_tool_prompt_line_for_agent(name, line, agent, store))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) struct InternalToolAvailability {
    browser_session_provider: bool,
    search_provider: bool,
    x_search_provider: bool,
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
    send_message: bool,
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
            x_search_provider: true,
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
            send_message: true,
        }
    }
}

impl Clone for InternalToolAvailability {
    fn clone(&self) -> Self {
        Self {
            browser_session_provider: self.browser_session_provider,
            search_provider: self.search_provider,
            x_search_provider: self.x_search_provider,
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
            send_message: self.send_message,
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
        x_search_provider: config.as_ref().is_some_and(x_search_credentials_configured),
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
        send_message: config
            .as_ref()
            .map(|config| config.chat.send_message_tool_enabled)
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

fn x_search_credentials_configured(config: &crate::models::AppConfig) -> bool {
    env::var("XAI_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        || config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| env.get("XAI_API_KEY"))
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        || x_search_oauth_credential_configured()
}

fn x_search_oauth_credential_configured() -> bool {
    let mut provider = crate::models::LlmProvider::default();
    provider.id = "xai-oauth".into();
    provider.name = "xAI OAuth".into();
    provider.provider_type = "xai-oauth".into();
    provider.preset = Some("xai-oauth".into());
    crate::hermes_auth::resolve_hermes_runtime_credential(&provider)
        .is_some_and(|credential| !credential.api_key.trim().is_empty())
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
        "web_search" => availability.search_provider,
        "x_search" => availability.x_search_provider,
        "image_generate" => availability.image_provider,
        "video_generate" => availability.video_provider,
        "vision_analyze" | "video_analyze" | "browser_vision" => availability.vision_provider,
        "text_to_speech" | "transcribe_audio" => availability.audio_provider,
        "voice_status" | "voice_playback" | "voice_recording" => true,
        "weather" => availability.weather,
        "ha_list_entities" | "ha_get_state" | "ha_list_services" | "ha_call_service" => {
            availability.homeassistant
        }
        "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "feishu_drive_update_comment_reaction"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment" => availability.feishu,
        "yb_query_group_info" | "yb_query_group_members" | "yb_send_dm" | "yb_send_sticker" => {
            availability.yuanbao_bridge
        }
        "yb_search_sticker" => availability.yuanbao_bridge || availability.yuanbao_stickers,
        "spotify_playback" | "spotify_devices" | "spotify_queue" | "spotify_search"
        | "spotify_playlists" | "spotify_albums" | "spotify_library" => availability.spotify,
        "discord" | "discord_admin" => availability.discord,
        "send_message" => availability.send_message,
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
            r#"- read_file: payload {"path":"relative/or/absolute/path","offset":1,"limit":500} reads line-numbered pages and returns sha256/modifiedUnixMs file state; use {"mode":"raw","maxChars":80000} for unnumbered full text or {"mode":"chars","charOffset":0,"charLimit":12000} for character slices. For local PDF attachments, call read_file first: text-based PDFs are extracted best-effort; if it reports scanned/encrypted/no extractable text, use the ocr-and-documents skill/OCR workflow. For remote PDF URLs, prefer web_extract first."#,
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
            r#"- write_file: payload {"path":"relative/or/absolute/path","content":"complete file content","expectedSha256":"optional sha256 from read_file","expectedModifiedUnixMs":123}. Include expected state when overwriting a file you read. Do not write temporary analysis scripts into the project source tree; use execute_code or an artifact/scratch path for one-off experiments."#,
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
            r#"- terminal: payload {"command":"shell command","cwd":".","stdin":"optional stdin text","taskId":"optional session","sessionId":"optional session","timeoutSeconds":180,"background":false,"notify_on_complete":false,"watch_patterns":["ready"]}. Timeout defaults to TERMINAL_TIMEOUT or 180s; explicit foreground timeout above TERMINAL_MAX_FOREGROUND_TIMEOUT (default 600s) is rejected, so use background=true with notify_on_complete=true for longer bounded jobs. With taskId/sessionId and no explicit cwd, SynthChat persists the shell CWD between terminal calls using a Hermes-style cwd marker. With background=true/backgroundProcess=true/bg=true, terminal is routed to process(action="start") so it returns a managed process session_id and supports notify_on_complete/watch_patterns, logs, wait, stdin, stop/kill, and notifications. Set TERMINAL_ENV=docker to run through the Docker backend with workspace and configured credential/skill/cache mounts, resource/security args, configured volumes/env, persistent labeled containers, cross-process reuse, and orphan cleanup. Set TERMINAL_ENV=singularity to execute through apptainer/singularity exec with workspace and configured credential/skill/cache bind mounts. Set TERMINAL_ENV=ssh with TERMINAL_SSH_HOST/USER/PORT/KEY to execute over SSH with stdin, timeout, ControlMaster reuse, remote cwd markers, credential/skill/cache upload sync unless TERMINAL_SSH_SYNC_FILES=false, and execution-time sync-back unless TERMINAL_SSH_SYNC_BACK=false; multi-file upload and sync-back use tar-over-SSH by default with scp fallback when disabled/unavailable, and stale synced remote files are removed unless TERMINAL_SSH_SYNC_DELETE=false. Set TERMINAL_ENV=modal with TERMINAL_MODAL_MODE=direct plus Modal credentials and the Python modal SDK for direct Modal sandbox execution with session cwd, app-data persisted snapshot restore/save, stale snapshot fallback to the base image, credential/skill/cache upload sync unless TERMINAL_MODAL_SYNC_FILES=false, and execution-time sync-back unless TERMINAL_MODAL_SYNC_BACK=false; set TERMINAL_MODAL_MODE=managed with a configured managed tool gateway/token for gateway-owned Modal terminal execution with remote cwd and environment snapshots. Set TERMINAL_ENV=daytona with DAYTONA_API_KEY and the Python daytona SDK for a basic persistent Daytona sandbox execution backend with credential/skill/cache upload sync unless TERMINAL_DAYTONA_SYNC_FILES=false and execution-time sync-back unless TERMINAL_DAYTONA_SYNC_BACK=false."#,
        ),
        (
            "process",
            r#"- process: payload {"action":"environment|environment_cleanup|checkpoint|recover|start|list|count|active|has_active|state|poll|log|wait|write|submit|close|stop|kill|stop_all|kill_all","command":"shell command","cwd":".","label":"dev server","processId":"...","taskId":"optional terminal session","sessionId":"...","session_id":"...","conversationId":"optional filter","runId":"optional filter","backend":"optional backend filter","envType":"optional env filter","data":"stdin text","timeoutSeconds":60,"offset":0,"limit":200,"forget":false,"notifyOnComplete":false,"watchPatterns":["ready"],"deleteSandbox":false}. environment reports Hermes-style TERMINAL_* backend config, requirements, remote sync files, local/SSH/Modal/Daytona terminal sessions, Modal persisted snapshot count, and Docker container lifecycle state; environment_cleanup stops matching SSH/Docker/Singularity/Modal/Daytona managed processes before tearing down backend state, runs SSH sync-back when active, stops or deletes Daytona sandboxes when TERMINAL_ENV=daytona, clears local/SSH/Modal/Daytona terminal state, clears Modal persisted snapshots, clears SSH sync state, and removes labeled Docker terminal containers for taskId/sessionId or all sessions. checkpoint reports and refreshes the Hermes-style processes.json metadata checkpoint for running managed processes; recover probes host PIDs and sandbox status_command entries, restoring live detached sessions that can be listed, polled, logged, killed, and reattached to the detached watcher. start/list/count/poll/log/wait/write/submit/close/stop/kill manage background processes; with TERMINAL_ENV=ssh, start launches via remote nohup and tracks a detached sandbox PID with status/kill/log tail over SSH, remote log cleanup on stop, but no stdin. With TERMINAL_ENV=docker, start reuses the labeled persistent Docker terminal container, launches via nohup in the container, and tracks a detached sandbox PID with docker exec status/kill/log tail and remote log cleanup, but no stdin. With TERMINAL_ENV=singularity, start launches a dedicated apptainer/singularity instance, runs nohup inside instance://..., and tracks a detached sandbox PID with exec status/kill/log tail and instance cleanup, but no stdin. With TERMINAL_ENV=modal, start creates a direct Modal sandbox, uploads configured sync files, launches via nohup, and tracks a detached sandbox PID with Modal SDK status/kill/log tail and sandbox cleanup, but no stdin. With TERMINAL_ENV=daytona, start creates or resumes the persistent Daytona sandbox, uploads configured sync files, launches via nohup, and tracks a detached sandbox PID with Daytona SDK status/kill/log tail and remote log cleanup, but no stdin. Detached SSH/Docker/Singularity/Modal/Daytona starts, explicit recover, and startup recovery attach one deduplicated Hermes-style poller per process id; the poller tails sandbox logs every ~2s, reads exit_command/exit file for sandbox exit codes, emits watch_match/watch_disabled events, and emits completed when notifyOnComplete=true or watch was disabled; startup reattach emits watchers_reattached. list and count return runningCount/running_count, exitedCount/exited_count, and hasActive/has_active, with taskId/sessionId, conversationId, runId, backend, and envType filters. stop_all/kill_all terminate all running managed processes matching taskId/sessionId, conversationId, runId, backend, or envType and emit one stopped event per process. list returns {"processes":[...],"count":N}; finished processes expose finishedAt/finished_at, are retained for about 30 minutes, and oldest finished entries are pruned once the registry exceeds 64 processes. kill is a Hermes-compatible alias for stop. Process snapshots include both camelCase and Hermes-style snake_case aliases such as session_id, task_id, backend, env_type, notify_on_complete, watch_patterns, exit_command, exit_code, stdout_tail, stderr_tail, conversation_id, and run_id. Use wait to block until a bounded job exits or times out; use log for paged stdout/stderr tail; use submit to write a line with newline. watchPatterns snapshots include watchStats/watch_stats with match/emit/drop counts, first/last match times, by-pattern/by-stream counters, and Hermes-style global flood counters globalSuppressedCount/globalTrippedCount. For bounded long tasks, prefer notifyOnComplete=true; for rare long-lived readiness signals, use watchPatterns. If neither is set, poll state/list/log or wait to avoid silent background jobs."#,
        ),
        (
            "execute_code",
            r#"- execute_code: payload {"language":"python|javascript|powershell","code":"print('ok')","cwd":".","taskId":"optional session","sessionId":"optional session","timeoutSeconds":60}. Local Python writes a short-lived workspace scratch file and exposes hermes_tools.py over loopback RPC so scripts can call web_search, web_extract, read_file, write_file, search_files, patch, and terminal. Prefer execute_code for throwaway PDF/document probes instead of creating read_pdf.py or similar files in the source tree. With TERMINAL_ENV=docker|ssh|singularity|modal|daytona, Python execute_code ships the script and hermes_tools.py to the selected backend and proxies those same tool calls through Hermes-style file RPC request/response files; non-Python remote languages run through the selected terminal backend using heredoc input so they share backend cwd/session, mounts, sync, timeout, and lifecycle behavior when that backend supports those features."#,
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
            "dashboard_auth",
            r#"- dashboard_auth: payload {"provider":"nous","action":"status|contract|diagnostics"} returns a read-only Hermes dashboard_auth/nous OAuth contract snapshot: client_id source/shape, Portal URL, authorize/token/JWKS endpoints, RS256 JWT claim expectations, no-refresh-token V1 behavior, redirect URI rules, and SynthChat desktop boundary notes. Use this when diagnosing dashboard OAuth readiness; it is separate from LLM `/auth login nous` device-code auth."#,
        ),
        (
            "dashboard_plugins",
            r#"- dashboard_plugins: payload {"action":"status|list|manifest|routes|achievements|state|diagnostics|rescan|reset-state|recent-unlocks|session-badges|fastapi-host|dashboard-host|host-plan|host-run|host-start|host-stop|host-restart|kanban-board|kanban-config|kanban-stats|kanban-assignees|kanban-task|kanban-events|kanban-events-checkpoint|kanban-runtime-events|kanban-runtime-checkpoint|kanban-create|kanban-update|kanban-delete|kanban-comment|kanban-link|kanban-unlink|kanban-bulk|kanban-diagnostics|kanban-workers-active|kanban-run|kanban-run-inspect|kanban-run-terminate|kanban-task-log|kanban-attachments|kanban-attachment-add|kanban-attachment-read|kanban-attachment-delete|kanban-dispatch|kanban-reclaim|kanban-reassign|kanban-boards|kanban-board-create|kanban-board-update|kanban-board-delete|kanban-board-switch|kanban-profiles|kanban-profile-update|kanban-profile-describe-auto|kanban-orchestration|kanban-orchestration-set","plugin":"all|hermes-achievements|example|kanban","sessionId":"optional","taskId":"optional","runId":"optional","queueItemId":"optional","tenant":"optional","includeArchived":false,"limit":20,"since":0,"board":"optional","dryRun":true,"enqueueAgent":false,"execute":false,"dashboardCommand":"optional","attachmentId":"optional","sourcePath":"optional","filename":"optional","profile":"optional","reclaimFirst":false} returns a Hermes dashboard plugin catalog/status snapshot for dashboard-only plugins such as hermes-achievements, example-dashboard, and kanban. fastapi-host/dashboard-host/host-plan expose the Hermes dashboard FastAPI host managed-process start/stop contract; host-start/host-run starts the external host, host-stop stops the tracked SynthChat managed-process task, and host-restart performs stop plus start through the async managed-process path when execute/live/apply is true. rescan performs a desktop-native Hermes achievements scan over SynthChat conversations/messages/runs/tool traces and writes Hermes-layout state.json, scan_snapshot.json, and scan_checkpoint.json; reset-state clears unlock/snapshot/checkpoint state; recent-unlocks and session-badges read the local snapshot. kanban-board, kanban-config, kanban-stats, kanban-assignees, and kanban-task adapt Hermes Kanban dashboard read routes from SynthChat AppStore/config, including columns, cards, dashboard preferences, comments, dependency links, and assignee/status counts. kanban-events and kanban-events-checkpoint adapt Hermes WebSocket /api/plugins/kanban/events?since= cursor payloads through desktop polling over task events; kanban-runtime-events and kanban-runtime-checkpoint merge queue items, AgentRun phase/tool transitions, ManagedProcess snapshots, and task events into one Hermes-style runtime cursor stream for dashboard/frontend consumption. kanban-create, kanban-update, kanban-delete, kanban-comment, kanban-link, kanban-unlink, and kanban-bulk adapt Hermes Kanban dashboard write routes to native AppStore task mutations. kanban-diagnostics, kanban-workers-active, kanban-run, kanban-run-inspect, kanban-run-terminate, and kanban-task-log adapt Hermes worker/diagnostic/readiness routes with SynthChat ManagedProcess and task metadata; kanban-attachments plus kanban-attachment-add/read/delete adapt Hermes attachment list/upload/download/delete routes as desktop file-backed actions; kanban-dispatch dry-runs by default and, with dryRun:false, claims ready assigned tasks into running AgentRunRecord/ManagedProcess entries for desktop worker visibility; with enqueueAgent:true it also appends a Kanban worker prompt into the normal agent queue for execution by the existing queue drain/runtime. kanban-reclaim and kanban-reassign adapt Hermes dashboard recovery routes by releasing active claims, stopping desktop managed worker records, aborting the matching AgentRunRecord when present, returning tasks to ready, and optionally assigning a new profile. kanban-boards, kanban-board-create/update/delete/switch, kanban-profiles, kanban-profile-update/describe-auto, and kanban-orchestration/set adapt Hermes board and orchestration settings through SynthChat config/personas/task metadata. These plugins are dashboard/API-host surfaces, not model tools, and SynthChat still does not embed Hermes' FastAPI dashboard host."#,
        ),
        (
            "api_server_daemon",
            r#"- api_server_daemon: payload {"action":"status|plan|start|run|stop|restart|daemon|managed-process","execute":false,"apiServerCommand":"optional"} exposes the Hermes API server daemon lifecycle as a desktop control surface. Read-only calls return the managed-process start/stop plan for running the external Hermes gateway with API_SERVER_ENABLED=true, the corresponding gateway service-manager/operator plan, and the native SynthChat HTTP/SSE boundary. With execute/live/apply:true the async dispatcher starts, stops, or restarts the external daemon through SynthChat's managed-process path so process logs, task ids, and stop controls are visible; OS service-manager install/start remains an operator-applied boundary."#,
        ),
        (
            "context_engine",
            r#"- context_engine: payload {"action":"status|discover|commands|diagnostics"} returns a Hermes context-engine plugin compatibility snapshot: default compressor engine, context.engine one-active-engine semantics, plugins/context_engine/<name> discovery status, register(ctx)/ContextEngine subclass loader patterns, context-engine slash-command forwarding rules, SynthChat native /context and /compact adaptation, compression auxiliary assignment status, dynamic context-engine command/tool discovery, bounded helper-subprocess command/tool dispatch, manual /compact and pre/post-turn lifecycle forwarding, and the remaining boundary that SynthChat does not embed Hermes' long-lived in-process Python ContextEngine object model."#,
        ),
        (
            "plugin_runtime",
            r#"- plugin_runtime: payload {"action":"status|sources|registries|commands|tools|hooks|auxiliary|diagnostics"} returns a Hermes PluginManager compatibility snapshot: bundled/user/project/entry-point discovery sources, PluginContext registration surface, Hermes load rules, current SynthChat plugin manifests, enabled plugin counts, Python plugin tool/command/skill/auxiliary discovery counts, manifest hook status, dynamic planner/dispatch bridge support, bounded helper-subprocess execution for plugin tools, slash commands, skills, auxiliary tasks, hooks, and context-engine bridges, plus the remaining boundary that SynthChat does not embed Hermes' byte-for-byte PluginManager or a long-lived Hermes Python daemon."#,
        ),
        (
            "teams_pipeline",
            r#"- teams_pipeline: payload {"action":"status|validate|list|show|subscriptions|token-health|upsert-subscription|delete-local-subscription|upsert-job|upsert-sink-record|get-sink-record|receipt-key|has-notification-receipt|record-notification-receipt|record-event-timestamp|get-event-timestamp|webhook-validation|webhook-notification|schedule-received|gateway-runtime|scheduler-runtime|runtime-plan|gateway-plan|gateway-stop|scheduler-stop|runtime-stop|gateway-restart|scheduler-restart|runtime-restart|fetch|run|summarize|generate-summary|summary-prompt|write-sinks|plan-sinks|subscribe|renew-subscription|delete-subscription|maintain-subscriptions","jobId":"optional","storePath":"optional","conversationId":"optional","personaId":"optional","execute":false,"dryRun":false,"enqueueAgent":false,"confirmPipelineRun":false,"confirmLiveGraphRead":false,"confirmLiveGraphMutation":false,"confirmSinkWrites":false,"summarizeWithLlm":false}. Hermes teams_pipeline desktop adaptation: reads and writes the durable TeamsPipelineStore-compatible local JSON state, validates/deduplicates MSGraph webhook notification batches into received pipeline jobs, locally schedule-received marks received jobs queued with deterministic agent prompts for the normal queue/cron/manual run surfaces, and with enqueueAgent:true appends those prompts to a SynthChat conversation and native agent queue without starting the run from this tool, exposes gateway-runtime/scheduler-runtime managed-process start/stop plans for the external Hermes MSGRAPH_WEBHOOK scheduler and can start, stop, or restart that scheduler through the async managed-process path when execute/live/apply is true, lists/compacts stored meeting jobs, shows subscriptions, validates MSGRAPH_* and Teams delivery readiness, reports token-health readiness, returns Graph artifact/replay/subscription plans by default, can perform live Microsoft Graph meeting artifact fetch only when execute/live/apply plus confirmLiveGraphRead are explicitly true, can run a confirmed native replay that fetches transcript artifacts, builds a Hermes-shaped TeamsMeetingSummaryPayload, and persists the job while planning Notion/Linear/Teams sink writes unless confirmSinkWrites is explicitly true; when run/replay also has summarizeWithLlm/useConfiguredLlmSummary:true the async dispatcher follows the live replay with current-agent LLM summary regeneration, persistence, and sink replay. It exposes Hermes JSON summary prompt/parser/fallback through summarize/generate-summary and can call the current agent LLM when summarize has execute/live/apply:true, can replay sink planning/writes for an existing completed job through write-sinks/plan-sinks, and can perform live Microsoft Graph subscription create/renew/delete/maintenance only when execute/live/apply plus confirmLiveGraphMutation are explicitly true and approvals allow the risky call. Hermes registered this plugin as operator CLI only; use terminal/process for full external sink replay when needed."#,
        ),
        (
            "teams_typing",
            r#"- teams_typing: payload {"action":"send|start|typing|stop","chat_id":"Bot Framework conversation id","conversation_id":"alias","serviceUrl":"optional Bot Framework service URL","timeoutMs":1500}. Sends a Hermes-style Microsoft Teams typing activity through Bot Framework POST /v3/conversations/{id}/activities using configured botToken/mediaAccessToken/accessToken or TEAMS_BOT_TOKEN/TEAMS_MEDIA_ACCESS_TOKEN/TEAMS_GRAPH_ACCESS_TOKEN. serviceUrl hosts are restricted to known Bot Framework hosts unless explicitly allowlisted; stop is a no-op because Bot Framework typing stops by ceasing refresh sends."#,
        ),
        (
            "mattermost_typing",
            r#"- mattermost_typing: payload {"action":"send|start|typing|stop","channel_id":"Mattermost channel id","chat_id":"alias"}. Sends a Hermes-style Mattermost typing indicator through POST /api/v4/users/{bot_user_id}/typing with {"channel_id":...}, using settings.mattermost.url/token and botUserId or MATTERMOST_BOT_USER_ID; if bot user id is absent it resolves /users/me first. stop is a no-op because Mattermost typing expires naturally."#,
        ),
        (
            "google_chat_typing",
            r#"- google_chat_typing: payload {"action":"send|start|typing|stop","chat_id":"spaces/<id>|users/<id>","target":"google_chat:spaces/<id>","thread_id":"optional spaces/<id>/threads/<id>","text":"optional marker text"}. Creates Hermes' visible Google Chat typing marker message (default "Hermes is thinking...") through POST /v1/{space_or_user}/messages using the same Google Chat REST credentials as send_message. stop is a no-op because the marker is a real message; live gateway runtimes may patch it in-place later."#,
        ),
        (
            "google_chat_update_message",
            r#"- google_chat_update_message: payload {"message_id":"spaces/<id>/messages/<id>","text":"replacement text"}. Adapts Hermes Google Chat edit_message/_patch_message by PATCHing /v1/{message_id}?updateMask=text with the same REST credentials as send_message, allowing a visible typing marker or progress message to be updated in-place without delete tombstones."#,
        ),
        (
            "provider_plugins",
            r#"- provider_plugins: payload {"family":"all|model|web|image|video","provider":"optional provider id"} returns a read-only Hermes provider-plugin catalog for model-providers, web providers, image_gen providers, and video_gen providers. It maps each Hermes manifest to SynthChat provider configuration/readiness, required env vars, missing env vars, and explicit runtime boundaries without making network calls or mutating provider settings."#,
        ),
        (
            "mcp_status",
            r#"- mcp_status: payload {} returns Hermes-style MCP configuration status: configured/enabled servers, registered MCP/utility tools, transport/protocol, auth hints, tool filters, parallel-safety flags, and needsRefresh markers. It is read-only and does not start MCP servers."#,
        ),
        (
            "mcp_oauth_clear",
            r#"- mcp_oauth_clear: payload {"serverId":"id/name prefix"} deletes the selected server's Hermes-layout MCP OAuth token triplet (<server>.json, <server>.client.json, <server>.meta.json) so the next use requires re-authentication. Use only after mcp_status shows needs_reauth or the user asks to reset MCP OAuth."#,
        ),
        (
            "mcp_oauth_refresh",
            r#"- mcp_oauth_refresh: payload {"serverId":"id/name prefix"} refreshes the selected server's Hermes-layout MCP OAuth token using cached refresh_token, client info, and OAuth metadata, then writes the refreshed token file. Use after mcp_status shows refresh_available, or when the user asks to refresh MCP OAuth. Do not use when tokenStatus.refreshReady=false; use mcp_oauth_clear only for reset/reauth."#,
        ),
        (
            "mcp_probe",
            r#"- mcp_probe: payload {"serverId":"optional id/name prefix","timeoutSeconds":10} explicitly starts enabled MCP server(s) long enough to list tools, applies include/exclude filters, updates the tool registry on success, and returns per-server ok/timedOut/error/toolCount diagnostics. Use mcp_status first."#,
        ),
        (
            "mcp_reset_session",
            r#"- mcp_reset_session: payload {"serverId":"optional id/name prefix"} closes the selected server's active MCP persistent stdio session, or all active persistent sessions when omitted. Use after mcp_status shows persistentSession.active=true and a persistent MCP call appears stale, wedged, or out of sync; the next tool call will start a fresh session."#,
        ),
        (
            "osv_check",
            r#"- osv_check: payload {"package":"@scope/pkg","ecosystem":"npm|PyPI","version":"optional"} or {"command":"npx|uvx|pipx","args":["pkg@1.0.0"]}. Queries OSV and reports MAL-* malware advisories only."#,
        ),
        (
            "security_scan",
            r#"- security_scan: payload {"content":"text or command","scope":"all|context|strict"} scans text with Hermes-style threat patterns and reports built-in command risk plus Tirith availability diagnostics."#,
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
            "kanban_decompose",
            r#"- kanban_decompose: payload {"objective":"larger task or goal","maxTasks":6,"create":false,"parents":["optional parent task ids"],"assignee":"optional"} decomposes work into actionable kanban draft cards using the kanban_decomposer auxiliary model when configured, otherwise deterministic fallback. Set create=true to create the cards."#,
        ),
        (
            "kanban_specify",
            r#"- kanban_specify: payload {"taskId":"triage task id","author":"optional"} expands a rough triage card into a concrete spec using the triage_specifier auxiliary model when configured, otherwise deterministic fallback, then promotes it to todo."#,
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
            "kanban_update",
            r#"- kanban_update: payload {"taskId":"task id","title":"optional","body":"optional","status":"triage|todo|scheduled|ready|running|blocked|review|done|archived","assignee":"optional/null","priority":0,"tenant":"optional/null","metadata":{}} updates a local agent kanban task and records an update event. done is stored as completed for compatibility with existing task tools."#,
        ),
        (
            "kanban_delete",
            r#"- kanban_delete: payload {"taskId":"task id","hardDelete":false} archives a kanban task by default; hardDelete=true removes it and clears dependency references."#,
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
            "kanban_unlink",
            r#"- kanban_unlink: payload {"parentId":"parent task id","childId":"child task id"} removes a kanban dependency link and records unlink events on both tasks."#,
        ),
        (
            "kanban_bulk_update",
            r#"- kanban_bulk_update: payload {"taskIds":["task ids"],"status":"optional","assignee":"optional/null","priority":0,"metadata":{},"author":"optional"} updates multiple kanban tasks with a dashboard-style bulk patch."#,
        ),
        (
            "send_message",
            r#"- send_message: payload {"action":"list"} returns local targets plus configured externalTargets and Hermes-style directoryTargets; payload {"action":"import_directory","directory":{"updated_at":"...","platforms":{"slack":[{"id":"C...","name":"engineering","type":"channel"}]}}} writes channel_directory.json; payload {"action":"refresh_directory","url":"https://...","token":"optional bearer","timeoutSeconds":15} fetches and writes channel_directory.json; payload {"action":"refresh_directory","platform":"mattermost"} builds channel_directory.json from the configured Mattermost teams/channels; payload {"target":"current|conversationId|title|discord|discord:<channel_id>|feishu:<receive_id>|feishu:<receive_id>:<reply_message_id>|telegram|telegram:<chat_id>|telegram:<chat_id>:<message_thread_id>|slack|slack:<channel_id>|slack:<channel_id>:<thread_ts>|slack:<user_id>|mattermost|mattermost:<channel_id>|mattermost:<channel_id>:<root_id>|matrix|matrix:<room_id>|signal|signal:<recipient>|signal:group:<group_id>|email|email:<address>|sms|sms:<phone>|dingtalk|dingtalk:<target>|whatsapp|whatsapp:<chat_id>|qqbot|qqbot:<id>|homeassistant|homeassistant:<notify_target>|bluebubbles|bluebubbles:<chat_id>|wecom:<chat_id>|weixin:<chat_id>|yuanbao:direct:<account_id>|yuanbao:group:<group_code>","message":"text to send","role":"assistant|user","platform":"optional discord|feishu|telegram|slack|mattermost|matrix|signal|email|sms|dingtalk|whatsapp|qqbot|homeassistant|bluebubbles|wecom|weixin|yuanbao","channel_id":"optional Discord/Telegram/Slack/Mattermost/QQBot channel id","chat_id":"optional Telegram/WhatsApp/QQBot/Home Assistant/BlueBubbles/WeCom/Weixin target","room_id":"optional Matrix room id","recipient":"optional Signal recipient","to":"optional Email/SMS target","subject":"optional Email subject","receive_id":"optional Feishu receive id","receive_id_type":"chat_id|open_id|union_id|email","user_id":"optional Yuanbao account id"} sends to local SynthChat conversations, Discord through configured bot/bridge, Feishu/Lark OpenAPI with MEDIA:<path> image/file uploads, Telegram Bot API with MEDIA:<path> photo/video/voice/audio/document uploads plus [[as_document]] force-document routing, Slack chat.postMessage text routing, Slack user IDs U... via conversations.open DM routing, Mattermost REST text posts and MEDIA:<path> local file uploads, Matrix Client-Server API routing with MEDIA:<path> uploads for unencrypted rooms, Signal signal-cli JSON-RPC with MEDIA:<path> attachments, Email SMTP text routing, SMS/Twilio text routing, DingTalk robot webhook text routing, WhatsApp bridge text routing, QQBot REST text routing, Home Assistant notify text routing, BlueBubbles iMessage text and MEDIA:<path> attachment routing, Yuanbao direct DM through the configured bridge, or WeCom/Weixin/Yuanbao group through settings.messagingGateway when configured. Bare platform targets require corresponding settings.* home target; named targets can resolve through Hermes channel_directory.json. In cron runs, duplicate sends to the configured HERMES_CRON_AUTO_DELIVER_* target are skipped because the final response will be auto-delivered there."#,
        ),
        (
            "session_search",
            r#"- session_search: payload {"query":"topic","limit":3,"kind":"all|message|run|tool|artifact|session_memory","sort":"newest|oldest"} discovers matching past sessions, runs, tool events, artifacts, and deleted-conversation session memory summaries; payload {} browses recent sessions; payload {"session_id":"...","around_message_id":"...","window":5} scrolls around an anchor. Use kind=session_memory to search conversation summaries saved when a chat was deleted; those results may have conversationDeleted=true and no scrollable session_id. Results include Hermes-style success/count/session_id/match_message_id/around_message_id aliases plus SynthChat conversationId/messageId fields."#,
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
            "memory_provider",
            r#"- memory_provider: payload {"action":"status|discover|tools"}. Hermes memory-provider adaptation: reports the active provider, bundled provider discovery, provider tool names, required env/config, local holographic state path, and external provider runtime boundaries."#,
        ),
        (
            "fact_store",
            r#"- fact_store: payload {"action":"add|search|probe|related|reason|contradict|update|remove|list","content":"fact text","query":"search text","entity":"entity","entities":["A","B"],"fact_id":"id","category":"user_pref|project|tool|general","tags":"comma,separated","trust_delta":0.1,"min_trust":0.3,"limit":10}. Hermes holographic memory adaptation with local structured facts, entity recall, trust scoring, and feedback."#,
        ),
        (
            "fact_feedback",
            r#"- fact_feedback: payload {"action":"helpful|unhelpful","fact_id":"id"}. Rates a fact_store fact after use and adjusts its trust score."#,
        ),
        (
            "supermemory_search",
            r#"- supermemory_search: payload {"query":"memory search","limit":5,"container_tag":"optional","execute":false,"confirmSupermemoryLive":false}. Hermes Supermemory provider semantic search. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmSupermemoryLive:true it performs the Supermemory v4 REST bridge matching Hermes SDK calls. supermemory_store/profile/forget share the same confirmed-live mode."#,
        ),
        (
            "honcho_reasoning",
            r#"- honcho_reasoning: payload {"query":"question about user/context","reasoning_level":"minimal|low|medium|high|max","peer":"user","execute":false,"confirmHonchoLive":false}. Hermes Honcho dialectic Q&A. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmHonchoLive:true it performs the Honcho v3 REST bridge using HONCHO_* / honcho.json config. honcho_profile/search/context/conclude share the same confirmed-live mode."#,
        ),
        (
            "mem0_search",
            r#"- mem0_search: payload {"query":"memory search","top_k":10,"rerank":false,"execute":false,"confirmMem0Live":false}. Hermes Mem0 semantic memory search. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmMem0Live:true it performs the Mem0 Platform v3 REST bridge matching Hermes MemoryClient calls. mem0_profile and mem0_conclude share the same confirmed-live mode."#,
        ),
        (
            "viking_search",
            r#"- viking_search: payload {"query":"knowledge search","mode":"auto|fast|deep","scope":"viking://optional","limit":10,"execute":false,"confirmOpenVikingLive":false}. Hermes OpenViking semantic search. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmOpenVikingLive:true it performs the OpenViking REST call using OPENVIKING_ENDPOINT/API key/account/user/agent. viking_read/browse/remember/add_resource share the same confirmed-live mode; viking_add_resource supports remote URLs/paths and local file multipart temp_upload before resource creation."#,
        ),
        (
            "byterover_status",
            r#"- byterover_status: payload {"action":"status|probe|run","execute":false,"confirmByteRoverLive":false}. Hermes ByteRover provider readiness/status for the brv CLI knowledge tree: reports brv CLI candidates, $HERMES_HOME/byterover working-directory stats, optional BRV_API_KEY cloud-sync readiness, and Hermes brv_query/brv_curate/brv_status contract. By default it does not execute brv; action=run or execute/live/apply:true plus confirmByteRoverLive:true runs brv status in the ByteRover working directory."#,
        ),
        (
            "brv_query",
            r#"- brv_query: payload {"query":"knowledge search","execute":false,"confirmByteRoverLive":false}. Hermes ByteRover persistent knowledge-tree search. By default this reports the planned brv query command without running external processes; with execute/live/apply:true plus confirmByteRoverLive:true it runs `brv query -- <query>` in $HERMES_HOME/byterover with Hermes' 10s query timeout and returns bounded stdout/stderr."#,
        ),
        (
            "brv_curate",
            r#"- brv_curate: payload {"content":"memory content","execute":false,"confirmByteRoverLive":false}. Hermes ByteRover persistent knowledge-tree write. By default this reports the planned brv curate command without running external processes; with execute/live/apply:true plus confirmByteRoverLive:true it runs `brv curate -- <content>` in $HERMES_HOME/byterover with Hermes' 120s curate timeout."#,
        ),
        (
            "brv_status",
            r#"- brv_status: payload {"execute":false,"confirmByteRoverLive":false}. Hermes ByteRover CLI status check. By default this reports the planned command and local readiness; with execute/live/apply:true plus confirmByteRoverLive:true it runs `brv status` in $HERMES_HOME/byterover with a 15s timeout."#,
        ),
        (
            "hindsight_search",
            r#"- hindsight_search: payload {"query":"memory search","max_tokens":4096,"tags":["optional"],"types":["observation"],"execute":false,"confirmHindsightLive":false}. Hermes Hindsight provider recall over knowledge-graph long-term memory. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmHindsightLive:true it performs a Hindsight REST bridge matching Hermes client.arecall bank/query/budget/max_tokens/tags/types semantics."#,
        ),
        (
            "hindsight_reflect",
            r#"- hindsight_reflect: payload {"query":"question to synthesize","budget":"low|mid|high","execute":false,"confirmHindsightLive":false}. Hermes Hindsight provider cross-memory reflection. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmHindsightLive:true it performs the Hindsight reflect REST bridge using HINDSIGHT_* / profile config."#,
        ),
        (
            "hindsight_remember",
            r#"- hindsight_remember: payload {"content":"memory content","context":"optional","tags":["optional"],"metadata":{},"execute":false,"confirmHindsightLive":false}. Hermes Hindsight provider memory write. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmHindsightLive:true it performs a Hindsight retain REST bridge matching Hermes client.aretain metadata/tag/context semantics."#,
        ),
        (
            "retaindb_search",
            r#"- retaindb_search: payload {"query":"memory search","limit":10,"memory_type":"optional","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB provider cloud-memory search. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs the Hermes POST /v1/memory/search request using RETAINDB_* config."#,
        ),
        (
            "retaindb_context",
            r#"- retaindb_context: payload {"query":"current task","max_tokens":1200,"execute":false,"confirmRetainDbLive":false}. Hermes RetainDB synthesized context query. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs POST /v1/context/query with include_memories=true."#,
        ),
        (
            "retaindb_store",
            r#"- retaindb_store: payload {"content":"memory content","memory_type":"fact|preference|task|project|context|relationship|custom","metadata":{},"execute":false,"confirmRetainDbLive":false}. Hermes RetainDB provider cloud-memory write. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs POST /v1/memory with Hermes fallback to POST /v1/memories."#,
        ),
        (
            "retaindb_remember",
            r#"- retaindb_remember: payload {"content":"memory content","memory_type":"factual|preference|goal|instruction|event|opinion","importance":0.7,"execute":false,"confirmRetainDbLive":false}. Hermes RetainDB explicit memory write alias; same confirmed live execution path as retaindb_store."#,
        ),
        (
            "retaindb_forget",
            r#"- retaindb_forget: payload {"memory_id":"memory id","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB delete-memory tool. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs DELETE /v1/memory/{memory_id} with fallback to DELETE /v1/memories/{memory_id}."#,
        ),
        (
            "retaindb_upload_file",
            r#"- retaindb_upload_file: payload {"local_path":"path","remote_path":"/optional/name","scope":"USER|PROJECT|ORG","ingest":false,"execute":false,"confirmRetainDbLive":false}. Hermes RetainDB shared file upload. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it uploads multipart POST /v1/files and can optionally ingest the returned file id."#,
        ),
        (
            "retaindb_list_files",
            r#"- retaindb_list_files: payload {"prefix":"optional","limit":50,"execute":false,"confirmRetainDbLive":false}. Hermes RetainDB shared file listing. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs GET /v1/files."#,
        ),
        (
            "retaindb_read_file",
            r#"- retaindb_read_file: payload {"file_id":"file id","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB shared file read. With confirmed live execution it reads metadata plus GET /v1/files/{file_id}/content and returns text up to 32000 chars or a binary-file note."#,
        ),
        (
            "retaindb_ingest_file",
            r#"- retaindb_ingest_file: payload {"file_id":"file id","user_id":"optional","agent_id":"optional","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB file ingestion. With confirmed live execution it performs POST /v1/files/{file_id}/ingest."#,
        ),
        (
            "retaindb_delete_file",
            r#"- retaindb_delete_file: payload {"file_id":"file id","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB shared file delete. With confirmed live execution it performs DELETE /v1/files/{file_id}."#,
        ),
        (
            "retaindb_ingest_session",
            r#"- retaindb_ingest_session: payload {"messages":[{"role":"user|assistant","content":"text"}],"user_id":"optional","session_id":"optional","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB durable session ingest. With confirmed live execution it performs POST /v1/memory/ingest/session with write_mode=sync."#,
        ),
        (
            "retaindb_agent_model",
            r#"- retaindb_agent_model: payload {"agent_id":"optional","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB agent self-model read. With confirmed live execution it performs GET /v1/memory/agent/{agent_id}/model."#,
        ),
        (
            "retaindb_seed_agent",
            r#"- retaindb_seed_agent: payload {"agent_id":"optional","content":"SOUL.md or persistent instructions","source":"soul_md","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB agent identity seed. With confirmed live execution it performs POST /v1/memory/agent/{agent_id}/seed."#,
        ),
        (
            "retaindb_profile",
            r#"- retaindb_profile: payload {"action":"status|profile","user_id":"optional","execute":false,"confirmRetainDbLive":false}. Hermes RetainDB provider profile/status. By default this reports readiness/route planning without network; with execute/live/apply:true plus confirmRetainDbLive:true it performs GET /v1/memory/profile/{user_id} with Hermes fallback to GET /v1/memories."#,
        ),
        (
            "skills_list",
            r#"- skills_list: payload {"query":"optional","category":"optional","enabledOnly":false}. Returns Hermes-style success, categories, and category fields."#,
        ),
        (
            "skill_view",
            r#"- skill_view: payload {"name":"skill id or name","filePath":"optional relative file","maxChars":20000}"#,
        ),
        (
            "skill_manage",
            r#"- skill_manage: payload {"action":"status|list_installs|usage|audit|audit_log|check_updates|update|uninstall|list_taps|add_tap|remove_tap|curator_report|curator_status|curator_pause|curator_resume|export_snapshot|import_snapshot|install_file|install_content|create|edit|patch|pin|unpin|archive|restore|delete|write_file|remove_file","name":"skill-name or selector when needed","content":"full SKILL.md","category":"optional","path":"snapshot or install file path","repo":"tap repo","filePath":"references/file.md","fileContent":"...","oldString":"...","newString":"...","replaceAll":false,"force":false,"reason":"archive reason"}. Use status/list_installs/usage/audit/check_updates before mutating skills; delete/uninstall/archive refuse unsafe targets. usage returns Hermes-style skill usage/provenance sidecar records used by curator protection."#,
        ),
        (
            "image_generate",
            r#"- image_generate: payload {"prompt":"image prompt","size":"1024x1024","n":1}. Uses the current persona image-generation settings for enablement, provider, model, style, and negative prompt."#,
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
            "voice_status",
            r#"- voice_status: payload {"cleanup":false,"maxAgeSeconds":3600} reports Hermes-style voice mode readiness: local audio capture/playback command availability, STT/TTS provider readiness, and optional old temp recording cleanup. It does not start recording."#,
        ),
        (
            "voice_playback",
            r#"- voice_playback: payload {"action":"play|stop|status","path":"relative audio path"} plays or interrupts a local audio file with the configured/default system player, matching Hermes-style voice playback controls."#,
        ),
        (
            "voice_recording",
            r#"- voice_recording: payload {"action":"start|stop|cancel|status","durationSeconds":0} starts, stops, cancels, or inspects a Hermes-style local voice recording. It writes a temporary recording_*.wav and requires HERMES_LOCAL_MIC_COMMAND/SYNTHCHAT_LOCAL_MIC_COMMAND or a platform recorder."#,
        ),
        (
            "meet_join",
            r#"- meet_join: payload {"url":"https://meet.google.com/abc-defg-hij","mode":"transcribe|realtime","guest_name":"Hermes Agent","duration":"30m","headed":false,"node":"optional","execute":false} validates an explicit Google Meet URL and records a Hermes-style desktop Meet session with transcript path and runtime boundary metadata, including the Hermes local Playwright bot subprocess command/env/log/status/transcript/say-queue plan plus SynthChat process-tool start/stop payloads keyed by google-meet:<meeting-id>. With execute/live/apply:true and no node, starts the planned local bot command through the managed process tool path so logs/status/stop controls appear in the normal process UI; google_meet.localBotCommand can override the command while preserving HERMES_MEET_* env setup. With node plus execute/live/apply:true, routes start_bot through the registered Hermes remote-node JSON-over-WebSocket RPC. It does not scan calendars or auto-dial; Playwright join/audio execution still happens inside the local google_meet runtime or an external meet node."#,
        ),
        (
            "meet_status",
            r#"- meet_status: payload {"node":"optional","execute":false} reports Hermes-style Google Meet session state, active flag, mode, transcript line count, transcript path, node hint, and desktop runtime boundary metadata. With node plus execute/live/apply:true, routes status through the registered Hermes remote-node RPC."#,
        ),
        (
            "meet_transcript",
            r#"- meet_transcript: payload {"last":10,"node":"optional","execute":false} reads the active Google Meet transcript file and returns all lines or the last N lines. With node plus execute/live/apply:true, routes transcript through the registered Hermes remote-node RPC."#,
        ),
        (
            "meet_leave",
            r#"- meet_leave: payload {"node":"optional","execute":false} marks the active Google Meet desktop session stopped and records a Hermes-style stop reason. With node plus execute/live/apply:true, routes stop through the registered Hermes remote-node RPC. Safe when no session is active."#,
        ),
        (
            "meet_say",
            r#"- meet_say: payload {"text":"what to say","node":"optional","execute":false} queues speech text for a realtime Google Meet session. With node plus execute/live/apply:true, routes say through the registered Hermes remote-node RPC. It refuses locally unless the active session was joined with mode='realtime'; actual audio playback requires the external google_meet OpenAI Realtime bridge."#,
        ),
        (
            "meet_node",
            r#"- meet_node: payload {"action":"list|status|approve|remove|resolve|request-envelope|token|ensure-token|requirements|setup|host-plan|run|audio-plan|realtime-plan","name":"node-name","url":"ws://host:18789","token":"secret","requestType":"ping|status|start_bot|stop|transcript|say","payload":{},"execute":false} manages the Hermes Google Meet remote-node registry at $HERMES_HOME/workspace/meetings/nodes.json, redacts tokens in read responses, can build Hermes JSON-over-WebSocket request envelopes, exposes Hermes meet-node host token/bootstrap commands, local Playwright/realtime setup requirements, and realtime audio bridge diagnostics. action=run with execute/live/apply:true starts the Hermes meet-node host through the normal managed process path; nodeHostCommand can override the launched command. Other execute/live/apply:true requests send one JSON-over-WebSocket request to the resolved remote node."#,
        ),
        (
            "disk_cleanup",
            r#"- disk_cleanup: payload {"action":"status|track|forget|dry_run|quick|deep|guess","path":"optional path","category":"temp|test|research|download|chrome-profile|cron-output|other"}. Hermes-style disk-cleanup adaptation for ephemeral session files: tracks scoped files under SynthChat data/HERMES_HOME, previews quick/deep cleanup, deletes deterministic test/temp/cron-output candidates with approval, and reports deep-clean prompt candidates instead of deleting them automatically."#,
        ),
        (
            "trace_flush",
            r#"- trace_flush: payload {"action":"status|flush|clear","dryRun":false}. Hermes Langfuse observability adaptation: reports buffered native trace events, flushes opt-in Langfuse events to /api/public/ingestion when HERMES_LANGFUSE_PUBLIC_KEY and HERMES_LANGFUSE_SECRET_KEY are configured, or clears the local buffer."#,
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
            "feishu_drive_update_comment_reaction",
            r#"- feishu_drive_update_comment_reaction: payload {"file_token":"doc file token","reply_id":"reply id","file_type":"docx","action":"add|delete","reaction_type":"OK"} adds or removes a Feishu/Lark Drive v2 comment reply reaction, matching Hermes feishu_comment reaction handling."#,
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
            "spotify_status",
            r#"- spotify_status: payload {"action":"status|manifest|tools|auth|diagnostics"} returns a read-only Hermes Spotify plugin status snapshot: plugin.yaml metadata, backend auto-load semantics, seven registered Spotify tools, Hermes `hermes auth spotify` / providers.spotify gate, SynthChat credential-source readiness from settings/env, risk-policy summary, and explicit no-network/no-token-refresh boundaries."#,
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
            r#"- artifact: payload {"name":"notes","content":"text to save"} or {"action":"publish_file","path":"workspace file","name":"optional"} publishes an existing workspace file as a clickable artifact. publish_file returns mediaTag as MEDIA:<path>; include it as its own line in the final reply to send the file through the linked WeChat bridge. The bridge hides this internal directive from visible text."#,
        ),
        (
            "document",
            r#"- document: payload {"title":"report title","format":"docx|xlsx|pptx|html|md|txt|csv","content":"document body","name":"optional file base name"}. Generates a common document artifact; docx/xlsx/pptx are real Office OpenXML files. Returns path, mimeType, and mediaTag as MEDIA:<path>. To send the file to a linked WeChat mobile user, include the returned mediaTag line in the final assistant reply. The bridge hides this internal directive from visible text."#,
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
            "browser_plugins",
            r#"- browser_plugins: payload {"action":"status|manifest|providers|readiness|diagnostics"} returns a read-only Hermes browser provider plugin snapshot for plugins/browser/browserbase, browser_use, and firecrawl: backend manifests, register_browser_provider semantics, required env readiness, Hermes legacy browser-use/browserbase selection behavior, Firecrawl explicit-selection boundary, SynthChat BrowserProvider registry mapping, and no-session/no-network diagnostic boundaries."#,
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
            r#"- x_search: payload {"query":"topic on X/Twitter","allowed_x_handles":["optional_handle"],"excluded_x_handles":["optional_handle"],"from_date":"YYYY-MM-DD","to_date":"YYYY-MM-DD","enable_image_understanding":false,"enable_video_understanding":false}. Uses xAI's built-in Responses x_search tool when xAI credentials are configured; set mode=web_search_bridge only for the legacy search-query bridge."#,
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
    let mcp_filters = registered_mcp_tool_filters(store)?;
    let platform_toolsets = cli_platform_toolsets(store)?;
    let mut tools = store
        .tool_definitions()?
        .into_iter()
        .filter(|tool| {
            agent.enabled_mcp_servers.is_empty()
                || agent.enabled_mcp_servers.contains(&tool.server_id)
        })
        .filter(|tool| registered_mcp_tool_allowed(tool, &mcp_filters))
        .filter(|tool| cli_platform_tool_allowed(tool, platform_toolsets.as_ref()))
        .collect::<Vec<_>>();
    tools.extend(
        python_plugin_tool_definitions(store)?
            .into_iter()
            .filter(|tool| {
                (agent.enabled_mcp_servers.is_empty()
                    || agent.enabled_mcp_servers.contains(&tool.server_id))
                    && cli_platform_tool_allowed(tool, platform_toolsets.as_ref())
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
            description: internal_tool_prompt_line_for_agent(name, line, agent, None)
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
        let mcp_filters = registered_mcp_tool_filters(store)?;
        let platform_toolsets = cli_platform_toolsets(store)?;
        tools.extend(store.tool_definitions()?.into_iter().filter(|tool| {
            (agent.enabled_mcp_servers.is_empty()
                || agent.enabled_mcp_servers.contains(&tool.server_id))
                && registered_mcp_tool_allowed(tool, &mcp_filters)
                && cli_platform_tool_allowed(tool, platform_toolsets.as_ref())
        }));
        tools.extend(
            python_plugin_tool_definitions(store)?
                .into_iter()
                .filter(|tool| {
                    (agent.enabled_mcp_servers.is_empty()
                        || agent.enabled_mcp_servers.contains(&tool.server_id))
                        && cli_platform_tool_allowed(tool, platform_toolsets.as_ref())
                }),
        );
    }
    let platform_toolsets = cli_platform_toolsets(store)?;
    tools.retain(|tool| cli_platform_tool_allowed(tool, platform_toolsets.as_ref()));
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

#[derive(Debug, Clone, Default)]
struct RegisteredMcpToolFilters {
    include: HashSet<String>,
    exclude: HashSet<String>,
}

fn registered_mcp_tool_filters(
    store: &AppStore,
) -> AppResult<HashMap<String, RegisteredMcpToolFilters>> {
    let mut filters = HashMap::new();
    for server in store.static_list("mcpServers")? {
        let Some(server_id) = server.get("id").and_then(Value::as_str) else {
            continue;
        };
        filters.insert(
            server_id.to_string(),
            RegisteredMcpToolFilters {
                include: registered_mcp_tool_filter_set(
                    server
                        .get("tools")
                        .and_then(|tools| tools.get("include"))
                        .or_else(|| server.get("toolInclude"))
                        .or_else(|| server.get("tool_include")),
                ),
                exclude: registered_mcp_tool_filter_set(
                    server
                        .get("tools")
                        .and_then(|tools| tools.get("exclude"))
                        .or_else(|| server.get("toolExclude"))
                        .or_else(|| server.get("tool_exclude")),
                ),
            },
        );
    }
    Ok(filters)
}

fn registered_mcp_tool_allowed(
    tool: &ToolDefinition,
    filters: &HashMap<String, RegisteredMcpToolFilters>,
) -> bool {
    if tool.source != "mcp" {
        return true;
    }
    let Some(filters) = filters.get(&tool.server_id) else {
        return true;
    };
    let name = normalize_registered_mcp_tool_filter_name(&tool.tool_name);
    (filters.include.is_empty() || filters.include.contains(&name))
        && !filters.exclude.contains(&name)
}

fn registered_mcp_tool_filter_set(value: Option<&Value>) -> HashSet<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(normalize_registered_mcp_tool_filter_name)
            .filter(|value| !value.is_empty())
            .collect(),
        Some(Value::String(raw)) => raw
            .split(',')
            .map(normalize_registered_mcp_tool_filter_name)
            .filter(|value| !value.is_empty())
            .collect(),
        _ => HashSet::new(),
    }
}

fn normalize_registered_mcp_tool_filter_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn cli_platform_toolsets(store: &AppStore) -> AppResult<Option<HashSet<String>>> {
    let config = store.config()?;
    let Some(value) = config
        .chat
        .auxiliary_task_assignments
        .get("hermesPlatformToolsets")
        .or_else(|| {
            config
                .chat
                .auxiliary_task_assignments
                .get("hermes_platform_toolsets")
        })
        .and_then(Value::as_object)
        .and_then(|object| object.get("cli").or_else(|| object.get("desktop")))
    else {
        return Ok(None);
    };
    Ok(Some(
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(super::normalize_toolset_name)
            .filter(|name| !name.is_empty())
            .collect(),
    ))
}

fn cli_platform_tool_allowed(
    tool: &ToolDefinition,
    platform_toolsets: Option<&HashSet<String>>,
) -> bool {
    let Some(platform_toolsets) = platform_toolsets else {
        return true;
    };
    if platform_toolsets.is_empty() {
        return false;
    }
    let toolsets = cli_platform_toolsets_for_tool(tool);
    toolsets.iter().any(|name| platform_toolsets.contains(name))
}

fn cli_platform_toolsets_for_tool(tool: &ToolDefinition) -> HashSet<String> {
    if tool.source == "internal" {
        let names = match tool.tool_name.as_str() {
            "terminal" | "process" | "env_probe" => vec!["terminal"],
            "execute_code" | "workspace_diagnostics" => vec!["code_execution"],
            _ => return tool_toolsets(tool),
        };
        return names.into_iter().map(str::to_string).collect();
    }
    tool_toolsets(tool)
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
    plugin_bridge_context: Option<&PythonPluginBridgeContext<'_>>,
) -> AppResult<(String, ToolEvent)> {
    let replay_payload = payload.clone();
    let payload = strip_provider_tool_call_metadata(payload);
    run_pre_tool_call_hooks(store, run_id, &definition.tool_name, &payload).await?;
    if definition
        .server_id
        .starts_with(PYTHON_PLUGIN_SERVER_PREFIX)
    {
        let started = Instant::now();
        let result = run_python_plugin_tool(
            store,
            &definition.tool_name,
            &payload,
            plugin_bridge_context,
        )
        .await;
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
        Some(run_id),
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
    let catalog = tool_catalog(store, agent, context, include_unavailable)?;
    let total_available = catalog.len();
    let retrieval = tool_catalog_retrieval_stats(&catalog, query);
    let mut matches = catalog
        .into_iter()
        .map(|entry| {
            let score = retrieval.score_entry(&entry);
            (score, entry)
        })
        .filter(|(score, _)| query.is_empty() || score.score > 0.0)
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .0
            .score
            .partial_cmp(&left.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.1["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right.1["name"].as_str().unwrap_or(""))
            })
    });
    if !query.is_empty() && matches.is_empty() {
        matches =
            retrieval.substring_matches(&tool_catalog(store, agent, context, include_unavailable)?);
        matches.sort_by(|left, right| {
            right
                .0
                .score
                .partial_cmp(&left.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left.1["name"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right.1["name"].as_str().unwrap_or(""))
                })
        });
    }
    let result_count = matches.len().min(limit);
    let matches = matches
        .into_iter()
        .take(limit)
        .map(|(score, mut entry)| {
            entry["score"] = json!(score.score);
            entry["searchMode"] = json!(score.mode);
            entry["search_mode"] = entry["searchMode"].clone();
            entry["matchedTerms"] = json!(score.matched_terms);
            entry["matched_terms"] = entry["matchedTerms"].clone();
            if entry.get("source_name").is_none() {
                entry["source_name"] = json!(tool_catalog_source_name(&entry));
            }
            if entry.get("sourceName").is_none() {
                entry["sourceName"] = entry.get("source_name").cloned().unwrap_or(Value::Null);
            }
            entry
        })
        .collect::<Vec<_>>();
    let retrieval_summary = json!({
        "mode": retrieval.mode(),
        "queryTokens": retrieval.query_tokens.clone(),
        "query_tokens": retrieval.query_tokens.clone(),
        "catalogSize": total_available,
        "catalog_size": total_available,
        "deferredCount": retrieval.deferred_count,
        "deferred_count": retrieval.deferred_count,
        "deferredTokens": retrieval.deferred_tokens,
        "deferred_tokens": retrieval.deferred_tokens,
        "thresholdTokens": retrieval.threshold_tokens,
        "threshold_tokens": retrieval.threshold_tokens,
        "thresholdPct": retrieval.threshold_pct,
        "threshold_pct": retrieval.threshold_pct,
        "returned": result_count,
        "limit": limit
    });
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "query": query,
        "includeUnavailable": include_unavailable,
        "total_available": total_available,
        "totalAvailable": total_available,
        "count": matches.len(),
        "searchMode": retrieval.mode(),
        "search_mode": retrieval.mode(),
        "deferredCount": retrieval.deferred_count,
        "deferred_count": retrieval.deferred_count,
        "deferredTokens": retrieval.deferred_tokens,
        "deferred_tokens": retrieval.deferred_tokens,
        "thresholdTokens": retrieval.threshold_tokens,
        "threshold_tokens": retrieval.threshold_tokens,
        "retrieval": retrieval_summary,
        "matches": matches
    }))?)
}

#[derive(Clone, Debug)]
struct ToolCatalogSearchScore {
    score: f64,
    mode: &'static str,
    matched_terms: Vec<String>,
}

#[derive(Clone, Debug)]
struct ToolCatalogRetrievalStats {
    query: String,
    query_tokens: Vec<String>,
    doc_tokens: Vec<Vec<String>>,
    doc_frequency: HashMap<String, usize>,
    avg_doc_len: f64,
    deferred_count: usize,
    deferred_tokens: usize,
    threshold_pct: f64,
    threshold_tokens: usize,
}

impl ToolCatalogRetrievalStats {
    fn mode(&self) -> &'static str {
        if self.query_tokens.is_empty() {
            "all"
        } else {
            "bm25"
        }
    }

    fn score_entry(&self, entry: &Value) -> ToolCatalogSearchScore {
        if self.query_tokens.is_empty() {
            return ToolCatalogSearchScore {
                score: 1.0,
                mode: "all",
                matched_terms: Vec::new(),
            };
        }
        let tokens = tool_catalog_search_tokens(entry);
        let score = bm25_score(
            &self.query_tokens,
            &tokens,
            self.avg_doc_len,
            &self.doc_frequency,
            self.doc_tokens.len(),
        ) + tool_catalog_name_boost(entry, &self.query_tokens);
        let matched_terms = matched_query_terms(&self.query_tokens, &tokens, entry);
        ToolCatalogSearchScore {
            score,
            mode: "bm25",
            matched_terms,
        }
    }

    fn substring_matches(&self, catalog: &[Value]) -> Vec<(ToolCatalogSearchScore, Value)> {
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        catalog
            .iter()
            .filter_map(|entry| {
                let haystack = tool_catalog_exact_fields(entry)
                    .into_iter()
                    .map(|value| value.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                if haystack.contains(&needle) {
                    Some((
                        ToolCatalogSearchScore {
                            score: 0.1,
                            mode: "substring",
                            matched_terms: vec![needle.clone()],
                        },
                        entry.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect()
    }
}

fn tool_catalog_retrieval_stats(catalog: &[Value], query: &str) -> ToolCatalogRetrievalStats {
    let doc_tokens = catalog
        .iter()
        .map(tool_catalog_search_tokens)
        .collect::<Vec<_>>();
    let mut doc_frequency = HashMap::new();
    for tokens in &doc_tokens {
        let mut seen = HashSet::new();
        for token in tokens {
            if seen.insert(token) {
                *doc_frequency.entry(token.clone()).or_insert(0) += 1;
            }
        }
    }
    let avg_doc_len = if doc_tokens.is_empty() {
        0.0
    } else {
        doc_tokens.iter().map(Vec::len).sum::<usize>() as f64 / doc_tokens.len() as f64
    };
    let deferred_tokens = estimate_tool_catalog_tokens(catalog);
    let threshold_pct = 10.0;
    let threshold_tokens = 20_000;
    ToolCatalogRetrievalStats {
        query: query.trim().to_string(),
        query_tokens: tokenize_tool_search_text(query),
        doc_tokens,
        doc_frequency,
        avg_doc_len,
        deferred_count: catalog.len(),
        deferred_tokens,
        threshold_pct,
        threshold_tokens,
    }
}

fn estimate_tool_catalog_tokens(catalog: &[Value]) -> usize {
    let total_chars = catalog
        .iter()
        .map(|entry| {
            serde_json::to_string(entry)
                .map(|text| text.len())
                .unwrap_or_else(|_| entry.to_string().len())
        })
        .sum::<usize>();
    (total_chars as f64 / TOOL_SEARCH_CHARS_PER_TOKEN).ceil() as usize
}

fn bm25_score(
    query_tokens: &[String],
    doc_tokens: &[String],
    avg_doc_len: f64,
    doc_frequency: &HashMap<String, usize>,
    doc_count: usize,
) -> f64 {
    if doc_tokens.is_empty() || query_tokens.is_empty() || doc_count == 0 {
        return 0.0;
    }
    let mut term_frequency = HashMap::<&str, usize>::new();
    for token in doc_tokens {
        *term_frequency.entry(token.as_str()).or_insert(0) += 1;
    }
    let k1 = 1.5;
    let b = 0.75;
    let doc_len = doc_tokens.len() as f64;
    let mut score = 0.0;
    for query_token in query_tokens {
        let Some(df) = doc_frequency.get(query_token).copied() else {
            continue;
        };
        let tf = term_frequency
            .get(query_token.as_str())
            .copied()
            .unwrap_or(0) as f64;
        if tf == 0.0 {
            continue;
        }
        let idf = (1.0 + (doc_count as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
        let denominator = tf + k1 * (1.0 - b + b * doc_len / avg_doc_len.max(1.0));
        score += idf * (tf * (k1 + 1.0) / denominator);
    }
    score
}

fn tool_catalog_name_boost(entry: &Value, query_tokens: &[String]) -> f64 {
    let exact_fields = tool_catalog_exact_fields(entry)
        .into_iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    query_tokens
        .iter()
        .map(|term| {
            exact_fields
                .iter()
                .map(|field| {
                    if field == term {
                        3.0
                    } else if field.contains(term) {
                        0.75
                    } else {
                        0.0
                    }
                })
                .fold(0.0, f64::max)
        })
        .sum()
}

fn matched_query_terms(
    query_tokens: &[String],
    doc_tokens: &[String],
    entry: &Value,
) -> Vec<String> {
    let doc_set = doc_tokens.iter().collect::<HashSet<_>>();
    let exact_text = tool_catalog_exact_fields(entry)
        .into_iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let mut matched = Vec::new();
    for term in query_tokens {
        if (doc_set.contains(term) || exact_text.contains(term)) && !matched.contains(term) {
            matched.push(term.clone());
        }
    }
    matched
}

fn tool_catalog_search_tokens(entry: &Value) -> Vec<String> {
    tokenize_tool_search_text(&tool_catalog_search_text(entry))
}

fn tool_catalog_search_text(entry: &Value) -> String {
    let mut parts = tool_catalog_exact_fields(entry);
    for key in ["description", "payloadShape"] {
        if let Some(value) = entry.get(key).and_then(Value::as_str) {
            parts.push(value.to_string());
        }
    }
    for key in ["payloadSchema", "inputSchema", "schema", "parameters"] {
        if let Some(value) = entry.get(key) {
            parts.extend(tool_schema_search_terms(value));
        }
    }
    parts.join(" ")
}

fn tool_catalog_exact_fields(entry: &Value) -> Vec<String> {
    let mut fields = [
        "name",
        "displayName",
        "source",
        "serverId",
        "toolName",
        "source_name",
        "sourceName",
    ]
    .into_iter()
    .filter_map(|key| entry.get(key).and_then(Value::as_str))
    .map(str::to_string)
    .collect::<Vec<_>>();
    if let Some(aliases) = entry.get("aliases").and_then(Value::as_array) {
        fields.extend(aliases.iter().filter_map(Value::as_str).map(str::to_string));
    }
    fields
}

fn tool_schema_search_terms(schema: &Value) -> Vec<String> {
    let mut terms = Vec::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        terms.extend(properties.keys().cloned());
        for value in properties.values() {
            if let Some(description) = value.get("description").and_then(Value::as_str) {
                terms.push(description.to_string());
            }
        }
    }
    if let Ok(text) = serde_json::to_string(schema) {
        terms.push(text);
    }
    terms
}

fn tokenize_tool_search_text(text: &str) -> Vec<String> {
    text.replace(['_', '.', '-', '/', ':'], " ")
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .flat_map(expand_tool_search_token)
        .collect()
}

fn expand_tool_search_token(token: String) -> Vec<String> {
    let mut tokens = vec![token.clone()];
    match token.as_str() {
        "visual" | "visually" => tokens.push("vision".into()),
        "vision" => tokens.push("visual".into()),
        "screenshot" | "screenshots" => {
            tokens.push("capture".into());
            tokens.push("image".into());
            tokens.push("vision".into());
            tokens.push("visual".into());
        }
        "capture" | "captures" => tokens.push("screenshot".into()),
        "image" | "images" => tokens.push("vision".into()),
        _ => {}
    }
    tokens
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
    let mut entry = entry;
    let parameters = entry
        .get("payloadSchema")
        .or_else(|| entry.get("inputSchema"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    entry["success"] = json!(true);
    entry["parameters"] = parameters.clone();
    entry["schema"] = parameters;
    entry["description"] = json!(description);
    if entry.get("source_name").is_none() {
        entry["source_name"] = json!(tool_catalog_source_name(&entry));
    }
    if entry.get("sourceName").is_none() {
        entry["sourceName"] = entry.get("source_name").cloned().unwrap_or(Value::Null);
    }
    Ok(serde_json::to_string_pretty(&entry)?)
}

fn tool_catalog_source_name(entry: &Value) -> String {
    let source = entry
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let server_id = entry
        .get("serverId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if server_id.is_empty() || server_id == "__internal" {
        source.to_string()
    } else {
        server_id.to_string()
    }
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
            description: internal_tool_prompt_line_for_agent(name, line, agent, Some(store))
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
            let rendered_line = internal_tool_prompt_line_for_agent(name, line, agent, Some(store));
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
    store: Option<&AppStore>,
) -> String {
    if name == "delegate_task" {
        return delegate_task_prompt_line(agent, store);
    }
    line.to_string()
}

fn delegate_task_prompt_line(agent: &AgentDefinition, store: Option<&AppStore>) -> String {
    let max_subagents = agent.max_subagents.max(1);
    let max_depth = agent.max_subagent_depth.max(1);
    let chat_config = store
        .and_then(|store| store.config().ok())
        .map(|config| config.chat);
    let max_concurrent_children = chat_config
        .as_ref()
        .map(|config| config.delegation_max_concurrent_children.max(1))
        .unwrap_or(3);
    let orchestrator_enabled = chat_config
        .as_ref()
        .map(|config| config.delegation_orchestrator_enabled)
        .unwrap_or(true);
    let delegation_strategy = chat_config
        .as_ref()
        .map(|config| config.delegation_strategy.trim())
        .filter(|strategy| !strategy.is_empty())
        .unwrap_or("auto");
    let batch_limit = max_subagents.min(max_concurrent_children);
    let nested = if max_depth > 1 && orchestrator_enabled {
        format!(
            "Nested delegation is enabled up to maxSubagentDepth={max_depth}; child agents may delegate only when payload.canDelegate=true and depth remains below the limit."
        )
    } else if max_depth > 1 {
        format!(
            "Nested delegation is disabled by delegationOrchestratorEnabled=false even though maxSubagentDepth={max_depth}; role=orchestrator is coerced to leaf."
        )
    } else {
        "Nested delegation is off for this agent; children are leaf workers and cannot call delegate_task."
            .into()
    };
    format!(
        r#"- delegate_task: single payload {{"task":"focused subtask","role":"researcher|planner|coder|orchestrator","toolsets":["file","browser"],"canDelegate":false}} or concurrent batch payload {{"tasks":[{{"goal":"subtask A","context":"needed details","toolsets":["file"],"role":"planner"}}]}}. Batch accepts up to min(maxSubagents, delegationMaxConcurrentChildren)={batch_limit} minus existing child runs. Current limits: maxSubagents={max_subagents}, maxSubagentDepth={max_depth}, delegationMaxConcurrentChildren={max_concurrent_children}, delegationOrchestratorEnabled={orchestrator_enabled}, delegationStrategy={delegation_strategy}. Subagent maxIterations is controlled by the active persona/Agent tool policy; caller-supplied maxIterations is ignored. {nested}"#
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
        | "feishu_drive_update_comment_reaction"
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
        "send_message" => "send_message is disabled in settings",
        _ => "tool availability check failed",
    }
}

fn payload_bool(payload: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
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
