mod agent;
mod error;
mod llm;
mod mcp;
mod model_catalog;
mod models;
mod plugins;
mod skills;
mod store;

use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use error::{AppError, AppResult};
use model_catalog::{ModelCapabilities, ModelCatalogEntry, ProviderCatalogInfo};
use models::{
    new_id, AgentDefinition, AppConfig, BrowserProvider, ImageProvider, LlmProvider, Persona,
    ProfileConfig, ScheduledAgentJob, ScheduledJobOutputRecord, SearchProvider, SendChatRequest,
    VideoProvider, VisionProvider,
};
use serde::Deserialize;
use serde_json::{json, Value};
use store::AppStore;
use tauri::{AppHandle, Emitter, Manager, State};

const REMOTE_SKILL_FETCH_TIMEOUT_SECS: u64 = 20;
const MAX_CHAT_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpCliAction {
    Stdio,
    Version,
    Check,
    Setup,
    SetupBrowser,
}

fn state_path() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("synthchat-data").join("state.json")
}

pub fn acp_stdio_requested_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    acp_cli_action_from_args(args) == Some(AcpCliAction::Stdio)
}

pub fn acp_cli_action_from_args<I, S>(args: I) -> Option<AcpCliAction>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().skip(1).find_map(|arg| match arg.as_ref() {
        "--acp-stdio" | "acp-stdio" | "serve-acp" | "--serve-acp" => Some(AcpCliAction::Stdio),
        "--version" => Some(AcpCliAction::Version),
        "--check" => Some(AcpCliAction::Check),
        "--setup" => Some(AcpCliAction::Setup),
        "--setup-browser" => Some(AcpCliAction::SetupBrowser),
        _ => None,
    })
}

pub fn print_acp_version() {
    println!("{}", env!("CARGO_PKG_VERSION"));
}

pub fn run_acp_check() -> AppResult<()> {
    let store = AppStore::new(state_path())?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": "check",
        "method": "initialize",
        "params": {}
    });
    let (_notifications, response) = agent::handle_acp_json_rpc_request(&store, &request)?;
    if response.get("error").is_some() {
        return Err(AppError::BadRequest(format!(
            "ACP initialize check failed: {response}"
        )));
    }
    println!("SynthChat ACP check OK");
    Ok(())
}

pub fn run_acp_setup() -> AppResult<()> {
    let store = AppStore::new(state_path())?;
    let provider_type = std::env::var("SYNTHCHAT_ACP_PROVIDER_TYPE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base_url = std::env::var("SYNTHCHAT_ACP_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let model = std::env::var("SYNTHCHAT_ACP_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let api_key_env = std::env::var("SYNTHCHAT_ACP_API_KEY_ENV")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let api_key = std::env::var("SYNTHCHAT_ACP_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if let (Some(provider_type), Some(base_url), Some(model)) =
        (provider_type.clone(), base_url.clone(), model.clone())
    {
        let provider = LlmProvider {
            id: "acp-runtime".into(),
            name: "ACP Runtime".into(),
            provider_type,
            preset: None,
            base_url,
            append_chat_path: true,
            api_key_env: api_key_env.unwrap_or_default(),
            api_key,
            model,
            enabled: true,
            ..LlmProvider::default()
        };
        store.set_providers(vec![provider])?;
        println!("SynthChat ACP setup OK");
        return Ok(());
    }

    let providers = store.providers()?;
    let configured = providers
        .iter()
        .filter(|provider| provider.enabled)
        .filter(|provider| !provider.model.trim().is_empty())
        .count();
    println!("SynthChat ACP setup");
    println!("Configured enabled providers: {configured}");
    println!(
        "To configure from this terminal, set SYNTHCHAT_ACP_PROVIDER_TYPE, SYNTHCHAT_ACP_BASE_URL, SYNTHCHAT_ACP_MODEL, and optionally SYNTHCHAT_ACP_API_KEY_ENV or SYNTHCHAT_ACP_API_KEY, then run --setup again."
    );
    if io::stdin().is_terminal() {
        println!("You can also open the SynthChat desktop settings page and configure the provider there.");
    }
    Ok(())
}

pub fn run_acp_setup_browser() -> AppResult<()> {
    println!(
        "SynthChat browser tools are configured from the desktop settings page. No terminal browser bootstrap is required."
    );
    Ok(())
}

pub fn run_acp_stdio() -> AppResult<()> {
    let store = AppStore::new(state_path())?;
    let runtime = tokio::runtime::Runtime::new()?;
    let stdin = io::stdin();
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let mut handles = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                let mut stdout = stdout
                    .lock()
                    .map_err(|_| AppError::BadRequest("ACP stdio stdout lock poisoned".into()))?;
                writeln!(
                    stdout,
                    "{}",
                    acp_stdio_error_response(Value::Null, -32700, &error.to_string())
                )?;
                stdout.flush()?;
                continue;
            }
        };
        let store = store.clone();
        let stdout = Arc::clone(&stdout);
        handles.push(runtime.spawn(async move {
            let notification_stdout = Arc::clone(&stdout);
            let notification_sink: agent::AcpNotificationSink = Arc::new(move |notification| {
                let mut stdout = notification_stdout
                    .lock()
                    .map_err(|_| AppError::BadRequest("ACP stdio stdout lock poisoned".into()))?;
                writeln!(stdout, "{notification}")?;
                stdout.flush()?;
                Ok(())
            });
            let result = agent::handle_acp_json_rpc_request_async_with_sink(
                &store,
                &request,
                Some(notification_sink),
            )
            .await;
            write_acp_stdio_result(stdout, request, result)
        }));
    }
    runtime.block_on(async {
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(error) => {
                    return Err(AppError::BadRequest(format!(
                        "ACP stdio task failed: {error}"
                    )))
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn write_acp_stdio_result(
    stdout: Arc<Mutex<io::Stdout>>,
    request: Value,
    result: AppResult<(Vec<Value>, Value)>,
) -> AppResult<()> {
    let mut stdout = stdout
        .lock()
        .map_err(|_| AppError::BadRequest("ACP stdio stdout lock poisoned".into()))?;
    match result {
        Ok((notifications, response)) => {
            for notification in notifications {
                writeln!(stdout, "{notification}")?;
            }
            writeln!(stdout, "{response}")?;
        }
        Err(error) => {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            writeln!(
                stdout,
                "{}",
                acp_stdio_error_response(id, -32603, &error.to_string())
            )?;
        }
    }
    stdout.flush()?;
    Ok(())
}

fn acp_stdio_error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[tauri::command(rename_all = "camelCase")]
fn get_config(store: State<'_, AppStore>) -> AppResult<AppConfig> {
    store.config()
}

#[tauri::command(rename_all = "camelCase")]
fn save_config(store: State<'_, AppStore>, config: AppConfig) -> AppResult<()> {
    store.set_config(config)
}

#[tauri::command(rename_all = "camelCase")]
fn add_trusted_tool_pattern(store: State<'_, AppStore>, pattern: String) -> AppResult<AppConfig> {
    store.trust_tool_pattern(pattern)
}

#[tauri::command(rename_all = "camelCase")]
fn remove_trusted_tool_pattern(
    store: State<'_, AppStore>,
    pattern: String,
) -> AppResult<AppConfig> {
    store.untrust_tool_pattern(&pattern)
}

#[tauri::command(rename_all = "camelCase")]
fn list_state_snapshots(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.state_snapshots()
}

#[tauri::command(rename_all = "camelCase")]
fn create_state_snapshot(store: State<'_, AppStore>, label: String) -> AppResult<Value> {
    store.create_state_snapshot(&label)
}

#[tauri::command(rename_all = "camelCase")]
fn prune_state_snapshots(store: State<'_, AppStore>, keep: usize) -> AppResult<usize> {
    store.prune_state_snapshots(keep)
}

#[tauri::command(rename_all = "camelCase")]
fn restore_state_snapshot(store: State<'_, AppStore>, snapshot_id: String) -> AppResult<Value> {
    store.restore_state_snapshot(&snapshot_id)
}

#[tauri::command(rename_all = "camelCase")]
fn list_workspace_snapshots(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.workspace_snapshots()
}

#[tauri::command(rename_all = "camelCase")]
fn create_workspace_snapshot(store: State<'_, AppStore>, label: String) -> AppResult<Value> {
    let root = std::env::current_dir()
        .map_err(|err| crate::error::AppError::BadRequest(format!("cannot resolve cwd: {err}")))?;
    store.create_workspace_snapshot(&label, &root)
}

#[tauri::command(rename_all = "camelCase")]
fn restore_workspace_snapshot(
    store: State<'_, AppStore>,
    snapshot_id: String,
    delete_new_files: bool,
) -> AppResult<Value> {
    store.restore_workspace_snapshot(&snapshot_id, delete_new_files)
}

#[tauri::command(rename_all = "camelCase")]
fn cleanup_historical_resources(store: State<'_, AppStore>) -> AppResult<Value> {
    store.cleanup_historical_resources()
}

#[tauri::command(rename_all = "camelCase")]
fn get_profile(store: State<'_, AppStore>) -> AppResult<ProfileConfig> {
    store.profile()
}

#[tauri::command(rename_all = "camelCase")]
fn save_profile(store: State<'_, AppStore>, profile: ProfileConfig) -> AppResult<ProfileConfig> {
    store.set_profile(profile)
}

#[tauri::command(rename_all = "camelCase")]
fn list_personas(store: State<'_, AppStore>) -> AppResult<Vec<Persona>> {
    store.personas()
}

#[tauri::command(rename_all = "camelCase")]
fn get_persona(store: State<'_, AppStore>, id: String) -> AppResult<Persona> {
    store.persona(Some(&id))
}

#[tauri::command(rename_all = "camelCase")]
fn save_persona(store: State<'_, AppStore>, persona: Persona) -> AppResult<Persona> {
    store.save_persona(persona)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_persona(_store: State<'_, AppStore>, _id: String) -> AppResult<()> {
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn list_conversations(store: State<'_, AppStore>) -> AppResult<Vec<models::Conversation>> {
    store.reload_from_disk()?;
    store.conversations()
}

#[tauri::command(rename_all = "camelCase")]
fn create_conversation(
    store: State<'_, AppStore>,
    title: Option<String>,
    persona_id: Option<String>,
) -> AppResult<models::Conversation> {
    store.create_conversation(title, persona_id)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_conversation(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    store.delete_conversation(&id)
}

#[tauri::command(rename_all = "camelCase")]
fn rename_conversation(store: State<'_, AppStore>, id: String, title: String) -> AppResult<()> {
    store.rename_conversation(&id, title)
}

#[tauri::command(rename_all = "camelCase")]
fn list_messages(
    store: State<'_, AppStore>,
    conversation_id: String,
    limit: Option<usize>,
    _preview_chars: Option<usize>,
) -> AppResult<Vec<models::ChatMessage>> {
    store.reload_from_disk()?;
    store.messages(&conversation_id, limit)
}

#[tauri::command(rename_all = "camelCase")]
async fn send_chat_message(
    app: AppHandle,
    store: State<'_, AppStore>,
    request: SendChatRequest,
) -> AppResult<Vec<models::ChatMessage>> {
    agent::run_chat_turn(&store, request, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
fn delete_message(_store: State<'_, AppStore>, _message_id: String) -> AppResult<()> {
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn list_llm_providers(store: State<'_, AppStore>) -> AppResult<Vec<LlmProvider>> {
    store.providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_llm_providers(store: State<'_, AppStore>, providers: Vec<LlmProvider>) -> AppResult<()> {
    store.set_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
async fn refresh_model_catalog(force_refresh: bool) -> AppResult<Value> {
    let catalog = model_catalog::fetch_models_dev_catalog(force_refresh).await?;
    let provider_count = catalog.as_object().map(|items| items.len()).unwrap_or(0);
    let model_count = catalog
        .as_object()
        .map(|providers| {
            providers
                .values()
                .filter_map(|provider| provider.get("models").and_then(Value::as_object))
                .map(|models| models.len())
                .sum::<usize>()
        })
        .unwrap_or(0);
    Ok(json!({
        "ok": true,
        "providerCount": provider_count,
        "modelCount": model_count
    }))
}

#[tauri::command(rename_all = "camelCase")]
fn lookup_model_capabilities(
    provider_id: String,
    model_id: String,
) -> AppResult<Option<ModelCapabilities>> {
    Ok(model_catalog::lookup_model_capabilities(
        &provider_id,
        &model_id,
    ))
}

#[tauri::command(rename_all = "camelCase")]
fn infer_provider_model_capabilities(provider: LlmProvider) -> AppResult<ModelCapabilities> {
    Ok(model_catalog::provider_model_capabilities(&provider))
}

#[tauri::command(rename_all = "camelCase")]
fn get_provider_catalog_info(provider_id: String) -> AppResult<Option<ProviderCatalogInfo>> {
    Ok(model_catalog::provider_catalog_info(&provider_id))
}

#[tauri::command(rename_all = "camelCase")]
fn list_agentic_models(provider_id: String) -> AppResult<Vec<ModelCatalogEntry>> {
    Ok(model_catalog::list_agentic_models(&provider_id))
}

#[tauri::command(rename_all = "camelCase")]
fn list_image_providers(store: State<'_, AppStore>) -> AppResult<Vec<ImageProvider>> {
    store.image_providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_image_providers(
    store: State<'_, AppStore>,
    providers: Vec<ImageProvider>,
) -> AppResult<()> {
    store.set_image_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_video_providers(store: State<'_, AppStore>) -> AppResult<Vec<VideoProvider>> {
    store.video_providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_video_providers(
    store: State<'_, AppStore>,
    providers: Vec<VideoProvider>,
) -> AppResult<()> {
    store.set_video_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_vision_providers(store: State<'_, AppStore>) -> AppResult<Vec<VisionProvider>> {
    store.vision_providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_vision_providers(
    store: State<'_, AppStore>,
    providers: Vec<VisionProvider>,
) -> AppResult<()> {
    store.set_vision_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_search_providers(store: State<'_, AppStore>) -> AppResult<Vec<SearchProvider>> {
    store.search_providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_search_providers(
    store: State<'_, AppStore>,
    providers: Vec<SearchProvider>,
) -> AppResult<()> {
    store.set_search_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_browser_providers(store: State<'_, AppStore>) -> AppResult<Vec<BrowserProvider>> {
    store.browser_providers()
}

#[tauri::command(rename_all = "camelCase")]
fn save_browser_providers(
    store: State<'_, AppStore>,
    providers: Vec<BrowserProvider>,
) -> AppResult<()> {
    store.set_browser_providers(providers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_mcp_servers(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.static_list("mcpServers")
}

#[tauri::command(rename_all = "camelCase")]
fn save_mcp_servers(store: State<'_, AppStore>, servers: Vec<Value>) -> AppResult<()> {
    store.set_mcp_servers(servers)
}

#[tauri::command(rename_all = "camelCase")]
fn list_capability_adapters(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::CapabilityAdapter>> {
    store.capability_adapters()
}

#[tauri::command(rename_all = "camelCase")]
fn save_capability_adapters(
    store: State<'_, AppStore>,
    adapters: Vec<models::CapabilityAdapter>,
) -> AppResult<Vec<models::CapabilityAdapter>> {
    store.set_capability_adapters(adapters)
}

#[tauri::command(rename_all = "camelCase")]
fn list_plugins(store: State<'_, AppStore>) -> AppResult<Vec<models::PluginSummary>> {
    plugins::list_plugins(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn toggle_plugin(
    store: State<'_, AppStore>,
    plugin_id: String,
    enabled: bool,
) -> AppResult<Vec<models::PluginSummary>> {
    plugins::toggle_plugin(&store, &plugin_id, enabled)
}

#[tauri::command(rename_all = "camelCase")]
async fn list_mcp_tools(
    store: State<'_, AppStore>,
    server_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<models::McpListToolsResult> {
    mcp::list_tools(&store, server_id, timeout_seconds).await
}

#[tauri::command(rename_all = "camelCase")]
async fn call_mcp_tool(
    store: State<'_, AppStore>,
    server_id: String,
    tool_name: String,
    payload: Value,
    timeout_seconds: Option<u64>,
) -> AppResult<models::McpCallResult> {
    let chat_config = store.config()?.chat;
    agent::call_mcp_tool_with_retry(
        &store,
        server_id,
        tool_name,
        payload,
        timeout_seconds,
        chat_config.tool_call_retry_count,
        chat_config.tool_call_retry_backoff_ms,
    )
    .await
}

#[tauri::command(rename_all = "camelCase")]
fn list_tool_traces(store: State<'_, AppStore>) -> AppResult<Vec<models::ToolTraceEntry>> {
    store.tool_traces()
}

#[tauri::command(rename_all = "camelCase")]
fn list_tool_definitions(store: State<'_, AppStore>) -> AppResult<Vec<models::ToolDefinition>> {
    store.tool_definitions()
}

#[tauri::command(rename_all = "camelCase")]
fn list_tool_approvals(store: State<'_, AppStore>) -> AppResult<Vec<models::ToolApprovalRequest>> {
    store.tool_approvals()
}

#[tauri::command(rename_all = "camelCase")]
async fn approve_tool_call(
    app: AppHandle,
    store: State<'_, AppStore>,
    approval_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<models::ToolApprovalRequest> {
    agent::approve_tool_call_and_resume(&store, approval_id, timeout_seconds, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
async fn approve_tool_call_always(
    app: AppHandle,
    store: State<'_, AppStore>,
    approval_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<models::ToolApprovalRequest> {
    agent::approve_tool_call_always_and_resume(&store, approval_id, timeout_seconds, Some(&app))
        .await
}

#[tauri::command(rename_all = "camelCase")]
async fn approve_tool_call_server(
    app: AppHandle,
    store: State<'_, AppStore>,
    approval_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<models::ToolApprovalRequest> {
    agent::approve_tool_call_server_and_resume(&store, approval_id, timeout_seconds, Some(&app))
        .await
}

#[tauri::command(rename_all = "camelCase")]
fn deny_tool_call(
    app: AppHandle,
    store: State<'_, AppStore>,
    approval_id: String,
    reason: Option<String>,
) -> AppResult<models::ToolApprovalRequest> {
    agent::deny_tool_call_and_update_run(&store, approval_id, reason, Some(&app))
}

#[tauri::command(rename_all = "camelCase")]
async fn refresh_tool_registry(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::ToolDefinition>> {
    mcp::refresh_tool_registry(&store).await
}

#[tauri::command(rename_all = "camelCase")]
fn list_planner_traces(store: State<'_, AppStore>) -> AppResult<Vec<models::PlannerTraceRecord>> {
    store.planner_traces()
}

#[tauri::command(rename_all = "camelCase")]
fn list_tool_router_traces(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::ToolRouterTraceRecord>> {
    store.tool_router_traces()
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_runs(store: State<'_, AppStore>) -> AppResult<Vec<models::AgentRunRecord>> {
    store.reload_from_disk()?;
    store.agent_runs()
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_control_commands() -> Vec<agent::AgentControlCommandView> {
    agent::list_agent_control_commands()
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_queue(store: State<'_, AppStore>) -> AppResult<Vec<models::AgentQueuedRequest>> {
    store.reload_from_disk()?;
    store.agent_queue()
}

#[tauri::command(rename_all = "camelCase")]
fn cancel_agent_queue_item(
    app: AppHandle,
    store: State<'_, AppStore>,
    id: String,
) -> AppResult<models::AgentQueuedRequest> {
    store.reload_from_disk()?;
    let item = store.cancel_agent_queue_item(&id)?;
    agent::emit_agent_queue_event(
        Some(&app),
        "canceled",
        Some(&item),
        Some(&item.conversation_id),
    );
    Ok(item)
}

#[tauri::command(rename_all = "camelCase")]
fn clear_finished_agent_queue_items(
    app: AppHandle,
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::AgentQueuedRequest>> {
    store.reload_from_disk()?;
    let items = store.clear_finished_agent_queue_items()?;
    agent::emit_agent_queue_event(Some(&app), "cleared", None, None);
    Ok(items)
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_todos(store: State<'_, AppStore>) -> AppResult<Vec<models::AgentTodoItem>> {
    store.agent_todos()
}

#[tauri::command(rename_all = "camelCase")]
fn list_scheduled_agent_jobs(store: State<'_, AppStore>) -> AppResult<Vec<ScheduledAgentJob>> {
    store.scheduled_agent_jobs()
}

#[tauri::command(rename_all = "camelCase")]
fn list_scheduled_job_outputs(
    store: State<'_, AppStore>,
    job_id: String,
) -> AppResult<Vec<ScheduledJobOutputRecord>> {
    store.scheduled_job_outputs(&job_id)
}

#[tauri::command(rename_all = "camelCase")]
fn save_scheduled_agent_job(
    store: State<'_, AppStore>,
    job: ScheduledAgentJob,
) -> AppResult<ScheduledAgentJob> {
    store.save_scheduled_agent_job(job)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_scheduled_agent_job(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    store.delete_scheduled_agent_job(&id)
}

#[tauri::command(rename_all = "camelCase")]
fn set_scheduled_agent_job_enabled(
    store: State<'_, AppStore>,
    id: String,
    enabled: bool,
) -> AppResult<ScheduledAgentJob> {
    store.set_scheduled_agent_job_enabled(&id, enabled)
}

#[tauri::command(rename_all = "camelCase")]
fn tick_scheduled_agent_jobs(
    app: AppHandle,
    store: State<'_, AppStore>,
) -> AppResult<Vec<ScheduledAgentJob>> {
    let Some(_lock) = store.try_acquire_cron_tick_lock()? else {
        return Ok(vec![]);
    };
    let due = store.claim_due_scheduled_agent_jobs()?;
    for job in &due {
        let conversation_id = match job.conversation_id.clone() {
            Some(id) if !id.trim().is_empty() => id,
            _ => {
                store
                    .create_conversation(Some(job.name.clone()), Some(job.persona_id.clone()))?
                    .id
            }
        };
        agent::spawn_background_chat_turn_for_job(
            app.clone(),
            conversation_id,
            job.persona_id.clone(),
            job.prompt.clone(),
            Some(job.clone()),
        );
    }
    Ok(due)
}

#[tauri::command(rename_all = "camelCase")]
fn export_agent_run_bundle(store: State<'_, AppStore>, run_id: String) -> AppResult<String> {
    agent::export_agent_run_bundle(&store, run_id)
}

#[tauri::command(rename_all = "camelCase")]
fn list_tool_artifacts_for_run(
    store: State<'_, AppStore>,
    run_id: String,
) -> AppResult<Vec<serde_json::Value>> {
    agent::list_agent_run_artifacts(&store, run_id)
}

#[tauri::command(rename_all = "camelCase")]
async fn drain_agent_queue(
    app: AppHandle,
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::AgentQueuedRequest>> {
    agent::drain_all_agent_queues(&store, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
async fn start_mattermost_adapter(app: AppHandle, store: State<'_, AppStore>) -> AppResult<Value> {
    agent::start_mattermost_adapter(&store, app).await
}

#[tauri::command(rename_all = "camelCase")]
fn stop_mattermost_adapter(store: State<'_, AppStore>) -> AppResult<Value> {
    agent::stop_mattermost_adapter(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn mattermost_adapter_status(store: State<'_, AppStore>) -> AppResult<Value> {
    agent::mattermost_adapter_status(&store)
}

#[tauri::command(rename_all = "camelCase")]
async fn start_platform_adapter(
    app: AppHandle,
    store: State<'_, AppStore>,
    platform: String,
) -> AppResult<Value> {
    agent::start_platform_adapter(&store, app, &platform).await
}

#[tauri::command(rename_all = "camelCase")]
fn stop_platform_adapter(store: State<'_, AppStore>, platform: String) -> AppResult<Value> {
    agent::stop_platform_adapter(&store, &platform)
}

#[tauri::command(rename_all = "camelCase")]
fn platform_adapter_status(
    store: State<'_, AppStore>,
    platform: Option<String>,
) -> AppResult<Value> {
    agent::platform_adapter_status(&store, platform.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
async fn resume_agent_run(
    app: AppHandle,
    store: State<'_, AppStore>,
    run_id: String,
    checkpoint_id: Option<String>,
) -> AppResult<models::AgentRunRecord> {
    agent::resume_agent_run(&store, run_id, checkpoint_id, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
async fn rerun_agent_run(
    app: AppHandle,
    store: State<'_, AppStore>,
    run_id: String,
) -> AppResult<Vec<models::ChatMessage>> {
    agent::rerun_agent_run(&store, run_id, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
async fn diagnose_agent_run(
    app: AppHandle,
    store: State<'_, AppStore>,
    run_id: String,
) -> AppResult<models::ChatMessage> {
    agent::diagnose_agent_run(&store, run_id, Some(&app)).await
}

#[tauri::command(rename_all = "camelCase")]
fn abort_agent_run(
    app: AppHandle,
    store: State<'_, AppStore>,
    run_id: String,
    reason: Option<String>,
) -> AppResult<models::AgentRunRecord> {
    agent::abort_agent_run(&store, run_id, reason, Some(&app))
}

#[tauri::command(rename_all = "camelCase")]
fn list_agents(store: State<'_, AppStore>) -> AppResult<Vec<AgentDefinition>> {
    store.agents()
}

#[tauri::command(rename_all = "camelCase")]
fn save_agent(store: State<'_, AppStore>, agent: AgentDefinition) -> AppResult<AgentDefinition> {
    store.save_agent(agent)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_agent(_store: State<'_, AppStore>, _id: String) -> AppResult<()> {
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn get_agent_config(store: State<'_, AppStore>) -> AppResult<Value> {
    Ok(agent_config_value(&store.agent(None)?))
}

#[tauri::command(rename_all = "camelCase")]
fn save_agent_config(store: State<'_, AppStore>, config: Value) -> AppResult<Value> {
    let mut agent = store.agent(None)?;
    if let Some(value) = config.get("enabled").and_then(Value::as_bool) {
        agent.enabled = value;
    }
    if let Some(value) = config.get("mcpEnabled").and_then(Value::as_bool) {
        agent.mcp_enabled = value;
    }
    if let Some(value) = config.get("skillsEnabled").and_then(Value::as_bool) {
        agent.skills_enabled = value;
    }
    if let Some(value) = config.get("allowShell").and_then(Value::as_bool) {
        agent.allow_shell = value;
    }
    if let Some(value) = config.get("maxSubagents").and_then(Value::as_u64) {
        agent.max_subagents = value.min(u32::MAX as u64) as u32;
    }
    if let Some(value) = config.get("maxSubagentDepth").and_then(Value::as_u64) {
        agent.max_subagent_depth = value.min(u32::MAX as u64) as u32;
    }
    if let Some(value) = config.get("maxToolIterations").and_then(Value::as_u64) {
        agent.max_tool_iterations = value.min(u32::MAX as u64) as u32;
    }
    if let Some(value) = config.get("skillsDir").and_then(Value::as_str) {
        agent.skills_dir = value.into();
    }
    if let Some(values) = config.get("enabledSkills").and_then(Value::as_array) {
        agent.enabled_skills = string_array_values(values);
    }
    if let Some(values) = config.get("enabledMcpServers").and_then(Value::as_array) {
        agent.enabled_mcp_servers = string_array_values(values);
    }
    if let Some(values) = config.get("enabledToolsets").and_then(Value::as_array) {
        agent.enabled_toolsets = string_array_values(values);
    }
    if let Some(values) = config.get("disabledToolsets").and_then(Value::as_array) {
        agent.disabled_toolsets = string_array_values(values);
    }
    let saved = store.save_agent(agent)?;
    Ok(agent_config_value(&saved))
}

fn agent_config_value(agent: &AgentDefinition) -> Value {
    json!({
        "enabled": agent.enabled,
        "mcpEnabled": agent.mcp_enabled,
        "skillsEnabled": agent.skills_enabled,
        "enabledMcpServers": agent.enabled_mcp_servers,
        "enabledToolsets": agent.enabled_toolsets,
        "disabledToolsets": agent.disabled_toolsets,
        "enabledSkills": agent.enabled_skills,
        "maxSubagents": agent.max_subagents,
        "maxSubagentDepth": agent.max_subagent_depth,
        "maxToolIterations": agent.max_tool_iterations,
        "allowShell": agent.allow_shell,
        "skillsDir": agent.skills_dir
    })
}

fn string_array_values(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[tauri::command(rename_all = "camelCase")]
fn list_skills(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    Ok(skills::list_skills(&store)?
        .into_iter()
        .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null))
        .collect())
}

#[tauri::command(rename_all = "camelCase")]
fn list_skills_for_agent(store: State<'_, AppStore>, agent_id: String) -> AppResult<Vec<Value>> {
    Ok(skills::list_skills_for_agent(&store, &agent_id)?
        .into_iter()
        .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null))
        .collect())
}

#[tauri::command(rename_all = "camelCase")]
fn install_builtin_skills(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    Ok(skills::install_builtin_skills(&store)?
        .into_iter()
        .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null))
        .collect())
}

#[tauri::command(rename_all = "camelCase")]
fn list_skill_bundles(store: State<'_, AppStore>) -> AppResult<Vec<models::SkillBundle>> {
    skills::list_skill_bundles(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn install_skill_bundle(
    store: State<'_, AppStore>,
    bundle_id: String,
    agent_id: Option<String>,
) -> AppResult<Vec<Value>> {
    Ok(
        skills::install_skill_bundle(&store, &bundle_id, agent_id.as_deref())?
            .into_iter()
            .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null))
            .collect(),
    )
}

#[tauri::command(rename_all = "camelCase")]
fn list_marketplace_skills(
    store: State<'_, AppStore>,
    query: Option<String>,
) -> AppResult<Vec<models::MarketplaceSkill>> {
    skills::list_marketplace_skills(&store, query.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn install_marketplace_skill(
    store: State<'_, AppStore>,
    skill_id: String,
    agent_id: Option<String>,
) -> AppResult<Option<Value>> {
    Ok(
        skills::install_marketplace_skill(&store, &skill_id, agent_id.as_deref())?
            .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null)),
    )
}

#[tauri::command(rename_all = "camelCase")]
fn audit_skills(
    store: State<'_, AppStore>,
    selector: Option<String>,
) -> AppResult<Vec<models::SkillAuditReport>> {
    skills::audit_skills(&store, selector.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn curate_skills(store: State<'_, AppStore>) -> AppResult<models::SkillCuratorReport> {
    skills::curate_skills_report(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn get_skill_curator_state(store: State<'_, AppStore>) -> AppResult<models::SkillCuratorState> {
    skills::skill_curator_state(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn set_skill_curator_paused(
    store: State<'_, AppStore>,
    paused: bool,
) -> AppResult<models::SkillCuratorState> {
    skills::set_skill_curator_paused(&store, paused)
}

#[tauri::command(rename_all = "camelCase")]
fn pin_skill_for_curator(
    store: State<'_, AppStore>,
    selector: String,
) -> AppResult<models::SkillCuratorState> {
    skills::pin_skill_for_curator(&store, &selector)
}

#[tauri::command(rename_all = "camelCase")]
fn unpin_skill_for_curator(
    store: State<'_, AppStore>,
    selector: String,
) -> AppResult<models::SkillCuratorState> {
    skills::unpin_skill_for_curator(&store, &selector)
}

#[tauri::command(rename_all = "camelCase")]
fn archive_skill_for_curator(
    store: State<'_, AppStore>,
    selector: String,
    reason: Option<String>,
) -> AppResult<models::SkillCuratorArchiveRecord> {
    skills::archive_skill_for_curator(&store, &selector, reason.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn restore_skill_for_curator(
    store: State<'_, AppStore>,
    selector: String,
) -> AppResult<models::SkillCuratorArchiveRecord> {
    skills::restore_skill_for_curator(&store, &selector)
}

#[tauri::command(rename_all = "camelCase")]
fn install_external_skill_file(
    store: State<'_, AppStore>,
    source_path: String,
    name: Option<String>,
    category: Option<String>,
    agent_id: Option<String>,
    force: Option<bool>,
) -> AppResult<Value> {
    Ok(serde_json::to_value(skills::install_external_skill_file(
        &store,
        &source_path,
        name.as_deref(),
        category.as_deref(),
        agent_id.as_deref(),
        force.unwrap_or(false),
    )?)
    .unwrap_or(Value::Null))
}

#[tauri::command(rename_all = "camelCase")]
async fn install_external_skill_url(
    store: State<'_, AppStore>,
    url: String,
    name: Option<String>,
    category: Option<String>,
    agent_id: Option<String>,
    force: Option<bool>,
) -> AppResult<Value> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(error::AppError::BadRequest(
            "skill url must start with http:// or https://".into(),
        ));
    }
    let raw = fetch_skill_url(trimmed).await?;
    let fallback = trimmed
        .rsplit('/')
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("external-skill")
        .trim_end_matches(".md");
    Ok(serde_json::to_value(skills::install_external_skill_content(
        &store,
        &raw,
        fallback,
        name.as_deref(),
        category.as_deref(),
        agent_id.as_deref(),
        force.unwrap_or(false),
        false,
        trimmed,
    )?)
    .unwrap_or(Value::Null))
}

#[tauri::command(rename_all = "camelCase")]
fn list_skill_install_records(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::SkillInstallRecord>> {
    skills::skill_install_records(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn list_skill_audit_log(store: State<'_, AppStore>, limit: Option<usize>) -> AppResult<Vec<Value>> {
    skills::skill_audit_log(&store, limit)
}

#[tauri::command(rename_all = "camelCase")]
fn list_skill_taps(store: State<'_, AppStore>) -> AppResult<Vec<models::SkillTap>> {
    skills::list_skill_taps(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn add_skill_tap(
    store: State<'_, AppStore>,
    repo: String,
    path: Option<String>,
) -> AppResult<models::SkillTap> {
    skills::add_skill_tap(&store, &repo, path.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn remove_skill_tap(store: State<'_, AppStore>, repo: String) -> AppResult<bool> {
    skills::remove_skill_tap(&store, &repo)
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubContentEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    download_url: Option<String>,
}

#[tauri::command(rename_all = "camelCase")]
async fn list_skill_tap_marketplace(
    store: State<'_, AppStore>,
    query: Option<String>,
) -> AppResult<Vec<models::MarketplaceSkill>> {
    list_tap_marketplace_skills(&store, query).await
}

#[tauri::command(rename_all = "camelCase")]
async fn search_skill_marketplace(
    store: State<'_, AppStore>,
    query: Option<String>,
    source: Option<String>,
) -> AppResult<Vec<models::MarketplaceSkill>> {
    let source = source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("local")
        .to_lowercase();
    let mut results = Vec::new();
    if source == "local" || source == "all" {
        results.extend(skills::list_marketplace_skills(&store, query.as_deref())?);
    }
    if source == "tap" || source == "taps" || source == "github" || source == "all" {
        results.extend(list_tap_marketplace_skills(&store, query).await?);
    }
    results.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    results.dedup_by(|a, b| a.id == b.id);
    Ok(results)
}

#[tauri::command(rename_all = "camelCase")]
async fn check_skill_taps(store: State<'_, AppStore>) -> AppResult<Vec<models::SkillTapStatus>> {
    let taps = skills::list_skill_taps(&store)?;
    let client = skill_http_client()?;
    let mut checks = Vec::new();
    for tap in taps {
        let path = tap.path.trim_end_matches('/').to_string();
        match fetch_github_contents(&client, &tap.repo, &path).await {
            Ok(entries) => checks.push(models::SkillTapStatus {
                repo: tap.repo,
                path: tap.path,
                status: "ok".into(),
                entry_count: entries.len(),
                detail: "tap path is readable".into(),
            }),
            Err(error) => checks.push(models::SkillTapStatus {
                repo: tap.repo,
                path: tap.path,
                status: "error".into(),
                entry_count: 0,
                detail: error.to_string(),
            }),
        }
    }
    Ok(checks)
}

async fn list_tap_marketplace_skills(
    store: &AppStore,
    query: Option<String>,
) -> AppResult<Vec<models::MarketplaceSkill>> {
    let taps = skills::list_skill_taps(store)?;
    let query = query
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    let client = skill_http_client()?;
    let mut results = Vec::new();
    for tap in taps {
        let mut stack = vec![(tap.path.trim_end_matches('/').to_string(), 0usize)];
        let mut visited = 0usize;
        while let Some((path, depth)) = stack.pop() {
            if visited >= 80 || depth > 3 {
                continue;
            }
            let Ok(entries) = fetch_github_contents(&client, &tap.repo, &path).await else {
                continue;
            };
            visited += entries.len();
            for entry in entries {
                if entry.entry_type == "dir" && depth < 3 {
                    stack.push((entry.path, depth + 1));
                    continue;
                }
                if entry.entry_type != "file" || !entry.name.eq_ignore_ascii_case("SKILL.md") {
                    continue;
                }
                let Some(download_url) = entry.download_url else {
                    continue;
                };
                let Ok(raw) = fetch_skill_url(&download_url).await else {
                    continue;
                };
                let id = format!(
                    "tap/{}/{}",
                    tap.repo,
                    entry.path.trim_end_matches("/SKILL.md")
                );
                let skill =
                    skills::marketplace_skill_from_remote_content(id, &raw, download_url, &tap);
                if query.as_deref().is_none_or(|query| {
                    [
                        skill.id.as_str(),
                        skill.name.as_str(),
                        skill.description.as_str(),
                        skill.author.as_str(),
                    ]
                    .iter()
                    .any(|value| value.to_lowercase().contains(query))
                }) {
                    results.push(skill);
                }
                if results.len() >= 50 {
                    break;
                }
            }
            if results.len() >= 50 {
                break;
            }
        }
    }
    results.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(results)
}

async fn fetch_github_contents(
    client: &reqwest::Client,
    repo: &str,
    path: &str,
) -> AppResult<Vec<GitHubContentEntry>> {
    let url = format!(
        "https://api.github.com/repos/{repo}/contents/{}",
        path.replace(' ', "%20")
    );
    let response = client
        .get(url)
        .header("User-Agent", "SynthChat-Skills-Tap")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("fetch tap contents failed: {error}")))?;
    let response = response
        .error_for_status()
        .map_err(|error| AppError::BadRequest(format!("fetch tap contents failed: {error}")))?;
    response
        .json::<Vec<GitHubContentEntry>>()
        .await
        .map_err(|error| AppError::BadRequest(format!("read tap contents failed: {error}")))
}

#[tauri::command(rename_all = "camelCase")]
fn check_skill_updates(
    store: State<'_, AppStore>,
    selector: Option<String>,
) -> AppResult<Vec<models::SkillUpdateCheck>> {
    skills::check_skill_updates(&store, selector.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn update_skills_from_sources(
    store: State<'_, AppStore>,
    selector: Option<String>,
    agent_id: Option<String>,
    force: Option<bool>,
) -> AppResult<Vec<Value>> {
    Ok(skills::update_skills_from_sources(
        &store,
        selector.as_deref(),
        agent_id.as_deref(),
        force.unwrap_or(false),
    )?
    .into_iter()
    .map(|skill| serde_json::to_value(skill).unwrap_or(Value::Null))
    .collect())
}

#[tauri::command(rename_all = "camelCase")]
async fn check_remote_skill_updates(
    store: State<'_, AppStore>,
    selector: Option<String>,
) -> AppResult<Vec<models::SkillUpdateCheck>> {
    let records = select_remote_skill_records(&store, selector.as_deref())?;
    let mut checks = Vec::new();
    for record in records {
        let raw = fetch_skill_url(&record.identifier).await?;
        let installed_raw = std::fs::read_to_string(&record.install_path).unwrap_or_default();
        let status = if stable_text_hash(&raw) == stable_text_hash(&installed_raw) {
            "current"
        } else {
            "update_available"
        };
        let detail = if status == "current" {
            "remote content matches installed content"
        } else {
            "remote content differs"
        };
        checks.push(models::SkillUpdateCheck {
            skill_id: record.skill_id,
            name: record.name,
            status: status.into(),
            detail: detail.into(),
        });
    }
    Ok(checks)
}

#[tauri::command(rename_all = "camelCase")]
async fn update_remote_skills_from_sources(
    store: State<'_, AppStore>,
    selector: Option<String>,
    agent_id: Option<String>,
    force: Option<bool>,
) -> AppResult<Vec<Value>> {
    let records = select_remote_skill_records(&store, selector.as_deref())?;
    let mut updated = Vec::new();
    for record in records {
        let raw = fetch_skill_url(&record.identifier).await?;
        let category = category_from_external_skill_id(&record.skill_id);
        let fallback = record
            .identifier
            .rsplit('/')
            .next()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("external-skill")
            .trim_end_matches(".md");
        let skill = skills::install_external_skill_content(
            &store,
            &raw,
            fallback,
            Some(&record.name),
            category.as_deref(),
            agent_id.as_deref(),
            force.unwrap_or(false),
            true,
            &record.identifier,
        )?;
        updated.push(serde_json::to_value(skill).unwrap_or(Value::Null));
    }
    Ok(updated)
}

fn select_remote_skill_records(
    store: &AppStore,
    selector: Option<&str>,
) -> AppResult<Vec<models::SkillInstallRecord>> {
    let selector = selector.map(str::trim).filter(|value| !value.is_empty());
    Ok(skills::skill_install_records(store)?
        .into_iter()
        .filter(|record| {
            record.identifier.starts_with("http://") || record.identifier.starts_with("https://")
        })
        .filter(|record| {
            selector.is_none_or(|selector| {
                let selector = selector.to_lowercase();
                record.skill_id.to_lowercase().starts_with(&selector)
                    || record.name.to_lowercase().starts_with(&selector)
            })
        })
        .collect())
}

async fn fetch_skill_url(url: &str) -> AppResult<String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(error::AppError::BadRequest(
            "skill url must start with http:// or https://".into(),
        ));
    }
    let response = skill_http_client()?
        .get(trimmed)
        .send()
        .await
        .map_err(|error| error::AppError::BadRequest(format!("fetch skill url failed: {error}")))?;
    if let Some(length) = response.content_length() {
        if length > 512 * 1024 {
            return Err(error::AppError::BadRequest(
                "skill url response is too large".into(),
            ));
        }
    }
    let response = response
        .error_for_status()
        .map_err(|error| error::AppError::BadRequest(format!("fetch skill url failed: {error}")))?;
    let raw = response
        .text()
        .await
        .map_err(|error| error::AppError::BadRequest(format!("read skill url failed: {error}")))?;
    if raw.len() > 512 * 1024 {
        return Err(error::AppError::BadRequest(
            "skill url response is too large".into(),
        ));
    }
    Ok(raw)
}

fn skill_http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(REMOTE_SKILL_FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|error| AppError::BadRequest(format!("build skill http client failed: {error}")))
}

fn category_from_external_skill_id(skill_id: &str) -> Option<String> {
    let parts = skill_id.split('/').collect::<Vec<_>>();
    if parts.len() <= 2 || parts.first() != Some(&"external") {
        return None;
    }
    Some(parts[1..parts.len() - 1].join("/"))
}

fn stable_text_hash(raw: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut hasher);
    hasher.finish()
}

#[tauri::command(rename_all = "camelCase")]
fn uninstall_external_skills(
    store: State<'_, AppStore>,
    selector: Option<String>,
    remove_files: Option<bool>,
) -> AppResult<Vec<models::SkillInstallRecord>> {
    skills::uninstall_external_skills(&store, selector.as_deref(), remove_files.unwrap_or(true))
}

#[tauri::command(rename_all = "camelCase")]
fn export_skill_snapshot(store: State<'_, AppStore>, path: String) -> AppResult<String> {
    skills::export_skill_snapshot(&store, &path)
}

#[tauri::command(rename_all = "camelCase")]
fn import_skill_snapshot(store: State<'_, AppStore>, path: String) -> AppResult<usize> {
    skills::import_skill_snapshot(&store, &path)
}

#[tauri::command(rename_all = "camelCase")]
fn save_skill_config(
    store: State<'_, AppStore>,
    agent_id: String,
    skill_id: String,
    config: HashMap<String, String>,
) -> AppResult<()> {
    skills::save_skill_config(&store, &agent_id, &skill_id, config)
}

#[tauri::command(rename_all = "camelCase")]
fn list_memories(
    store: State<'_, AppStore>,
    persona_id: Option<String>,
) -> AppResult<Vec<models::MemoryEntry>> {
    store.memories(persona_id.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn get_memory_status(
    store: State<'_, AppStore>,
    persona_id: Option<String>,
) -> AppResult<models::MemoryStatus> {
    let persona = store.persona(persona_id.as_deref())?;
    let memories = store.memories(Some(&persona.id))?;
    let prompt_safe = memories
        .iter()
        .filter(|memory| store::scan_memory_content(&memory.summary).is_none())
        .count();
    let blocked_by_security_scan = memories.len().saturating_sub(prompt_safe);
    let enabled = persona
        .memory
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_in_prompt = persona
        .memory
        .get("includeInPrompt")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let trigger_rounds = persona
        .memory
        .get("triggerRounds")
        .and_then(Value::as_u64)
        .unwrap_or(10);
    let max_memories = persona
        .memory
        .get("maxMemories")
        .and_then(Value::as_u64)
        .unwrap_or(50);
    let prompt_injected = if enabled && include_in_prompt {
        prompt_safe.min(max_memories.max(1) as usize)
    } else {
        0
    };
    Ok(models::MemoryStatus {
        persona_id: persona.id,
        persona_name: persona.name,
        enabled,
        include_in_prompt,
        trigger_rounds,
        max_memories,
        total: memories.len(),
        prompt_safe,
        blocked_by_security_scan,
        prompt_injected,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn save_memory(store: State<'_, AppStore>, memory: Value) -> AppResult<models::MemoryEntry> {
    let entry = models::MemoryEntry {
        id: memory
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        persona_id: memory
            .get("personaId")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string(),
        summary: memory
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        importance: memory
            .get("importance")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 5) as u8,
        created_at: memory
            .get("createdAt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        updated_at: memory
            .get("updatedAt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    if entry.summary.is_empty() {
        return Err(error::AppError::BadRequest(
            "memory summary is required".into(),
        ));
    }
    store.save_memory(entry)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_memory(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    store.delete_memory(&id)
}

#[tauri::command(rename_all = "camelCase")]
fn list_worldbooks(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.static_list("worldbooks")
}

#[tauri::command(rename_all = "camelCase")]
fn save_worldbook(store: State<'_, AppStore>, book: Value) -> AppResult<Value> {
    store.save_worldbook(book)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_worldbook(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    store.delete_worldbook(&id)
}

#[tauri::command(rename_all = "camelCase")]
fn list_themes(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.static_list("themes")
}

#[tauri::command(rename_all = "camelCase")]
fn save_themes(_store: State<'_, AppStore>, themes: Vec<Value>) -> Vec<Value> {
    themes
}

#[tauri::command(rename_all = "camelCase")]
fn get_token_usage_stats(store: State<'_, AppStore>) -> AppResult<Value> {
    store.token_usage()
}

#[tauri::command(rename_all = "camelCase")]
fn get_short_context_state(
    store: State<'_, AppStore>,
    conversation_id: String,
) -> AppResult<models::ShortContextState> {
    store.short_context(&conversation_id)
}

#[tauri::command(rename_all = "camelCase")]
fn upload_chat_attachment(
    store: State<'_, AppStore>,
    file_name: String,
    mime_type: String,
    bytes: Vec<u8>,
) -> AppResult<Value> {
    if bytes.len() > MAX_CHAT_ATTACHMENT_BYTES {
        return Err(AppError::BadRequest(format!(
            "attachment too large: {} bytes",
            bytes.len()
        )));
    }
    let safe_name = sanitize_attachment_file_name(&file_name);
    let id = new_id("attachment");
    let attachment_dir = store.data_dir().join("attachments");
    fs::create_dir_all(&attachment_dir)?;
    let path = attachment_dir.join(format!("{id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    Ok(json!({
        "id": id,
        "fileName": file_name,
        "mimeType": mime_type,
        "fileSize": bytes.len(),
        "path": path.to_string_lossy().to_string(),
    }))
}

fn sanitize_attachment_file_name(file_name: &str) -> String {
    let path = PathBuf::from(file_name);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let safe = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe = safe.trim_matches(['.', '_', '-']).to_string();
    if safe.is_empty() {
        "attachment".into()
    } else {
        safe
    }
}

#[tauri::command(rename_all = "camelCase")]
fn environment_check(store: State<'_, AppStore>) -> AppResult<Value> {
    let providers = store.providers()?;
    let has_real_provider = providers
        .iter()
        .any(|p| p.enabled && p.provider_type != "echo" && !p.base_url.trim().is_empty());
    let items = vec![
        json!({
            "id": "rust-backend",
            "name": "Rust 对话链",
            "status": "ok",
            "detail": "Tauri Rust backend is active."
        }),
        json!({
            "id": "llm-provider",
            "name": "LLM Provider",
            "status": if has_real_provider { "ok" } else { "missing" },
            "detail": if has_real_provider { "已配置真实模型服务。" } else { "当前使用本地 echo fallback，可在设置中配置 OpenAI-compatible 或 Ollama。"},
            "fixAction": null,
            "fixLabel": null
        }),
    ];
    Ok(json!({"items": items, "allPassed": has_real_provider}))
}

#[tauri::command(rename_all = "camelCase")]
fn empty_list() -> Vec<Value> {
    vec![]
}

#[tauri::command(rename_all = "camelCase")]
fn noop() {}

#[tauri::command(rename_all = "camelCase")]
fn passthrough_value(value: Value) -> Value {
    value
}

#[tauri::command(rename_all = "camelCase")]
fn asset_url(path: String) -> String {
    path
}

#[tauri::command(rename_all = "camelCase")]
fn open_local_file(path: String) -> AppResult<()> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(error::AppError::NotFound(format!(
            "local file not found: {}",
            path.display()
        )));
    }
    #[cfg(target_os = "windows")]
    Command::new("cmd")
        .args(["/C", "start", "", &path.to_string_lossy()])
        .spawn()?;
    #[cfg(target_os = "macos")]
    Command::new("open").arg(&path).spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    Command::new("xdg-open").arg(&path).spawn()?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn reveal_local_file(path: String) -> AppResult<()> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(error::AppError::NotFound(format!(
            "local file not found: {}",
            path.display()
        )));
    }
    #[cfg(target_os = "windows")]
    Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn()?;
    #[cfg(target_os = "macos")]
    Command::new("open")
        .args(["-R", &path.to_string_lossy()])
        .spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    Command::new("xdg-open")
        .arg(path.parent().unwrap_or_else(|| std::path::Path::new(".")))
        .spawn()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = AppStore::new(state_path()).expect("failed to initialize SynthChat state");
    tauri::Builder::default()
        .manage(store)
        .setup(|app| {
            let store = app.state::<AppStore>();
            let reattached = agent::reattach_managed_process_watchers(&store, Some(&app.handle()));
            if reattached > 0 {
                let _ = app.emit(
                    "synthchat-managed-process-event",
                    json!({
                        "type": "watchers_reattached",
                        "detail": {
                            "count": reattached,
                            "source": "startup_recover",
                        },
                    }),
                );
            }
            if let Ok(started_adapters) =
                agent::start_configured_platform_adapters(&store, app.handle().clone())
            {
                if !started_adapters.is_empty() {
                    let _ = app.emit(
                        "synthchat-platform-adapter-event",
                        json!({
                            "type": "autostart_requested",
                            "detail": {
                                "platforms": started_adapters,
                                "source": "startup",
                            },
                        }),
                    );
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            add_trusted_tool_pattern,
            remove_trusted_tool_pattern,
            list_state_snapshots,
            create_state_snapshot,
            prune_state_snapshots,
            restore_state_snapshot,
            list_workspace_snapshots,
            create_workspace_snapshot,
            restore_workspace_snapshot,
            cleanup_historical_resources,
            get_profile,
            save_profile,
            list_personas,
            get_persona,
            save_persona,
            delete_persona,
            list_conversations,
            create_conversation,
            delete_conversation,
            rename_conversation,
            list_messages,
            send_chat_message,
            delete_message,
            list_llm_providers,
            save_llm_providers,
            refresh_model_catalog,
            lookup_model_capabilities,
            infer_provider_model_capabilities,
            get_provider_catalog_info,
            list_agentic_models,
            list_image_providers,
            save_image_providers,
            list_video_providers,
            save_video_providers,
            list_vision_providers,
            save_vision_providers,
            list_search_providers,
            save_search_providers,
            list_browser_providers,
            save_browser_providers,
            list_mcp_servers,
            save_mcp_servers,
            list_capability_adapters,
            save_capability_adapters,
            list_plugins,
            toggle_plugin,
            list_mcp_tools,
            call_mcp_tool,
            list_tool_traces,
            list_tool_definitions,
            list_tool_approvals,
            approve_tool_call,
            approve_tool_call_always,
            approve_tool_call_server,
            deny_tool_call,
            refresh_tool_registry,
            list_planner_traces,
            list_tool_router_traces,
            list_agent_runs,
            list_agent_control_commands,
            list_agent_queue,
            cancel_agent_queue_item,
            clear_finished_agent_queue_items,
            list_agent_todos,
            list_scheduled_agent_jobs,
            list_scheduled_job_outputs,
            save_scheduled_agent_job,
            delete_scheduled_agent_job,
            set_scheduled_agent_job_enabled,
            tick_scheduled_agent_jobs,
            export_agent_run_bundle,
            list_tool_artifacts_for_run,
            drain_agent_queue,
            start_mattermost_adapter,
            stop_mattermost_adapter,
            mattermost_adapter_status,
            start_platform_adapter,
            stop_platform_adapter,
            platform_adapter_status,
            resume_agent_run,
            rerun_agent_run,
            diagnose_agent_run,
            abort_agent_run,
            list_agents,
            save_agent,
            delete_agent,
            get_agent_config,
            save_agent_config,
            list_skills,
            list_skills_for_agent,
            install_builtin_skills,
            list_skill_bundles,
            install_skill_bundle,
            list_marketplace_skills,
            install_marketplace_skill,
            audit_skills,
            curate_skills,
            get_skill_curator_state,
            set_skill_curator_paused,
            pin_skill_for_curator,
            unpin_skill_for_curator,
            archive_skill_for_curator,
            restore_skill_for_curator,
            install_external_skill_file,
            install_external_skill_url,
            list_skill_install_records,
            list_skill_audit_log,
            list_skill_taps,
            add_skill_tap,
            remove_skill_tap,
            list_skill_tap_marketplace,
            search_skill_marketplace,
            check_skill_taps,
            check_skill_updates,
            update_skills_from_sources,
            check_remote_skill_updates,
            update_remote_skills_from_sources,
            uninstall_external_skills,
            export_skill_snapshot,
            import_skill_snapshot,
            save_skill_config,
            list_memories,
            get_memory_status,
            save_memory,
            delete_memory,
            list_worldbooks,
            save_worldbook,
            delete_worldbook,
            list_themes,
            save_themes,
            get_token_usage_stats,
            get_short_context_state,
            upload_chat_attachment,
            environment_check,
            empty_list,
            noop,
            passthrough_value,
            asset_url,
            open_local_file,
            reveal_local_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_file_name_sanitizer_strips_paths_and_unsafe_chars() {
        assert_eq!(
            sanitize_attachment_file_name("../../bad name.png"),
            "bad_name.png"
        );
        assert_eq!(sanitize_attachment_file_name("..."), "attachment");
    }

    #[test]
    fn acp_stdio_flag_is_detected_from_args() {
        assert!(acp_stdio_requested_from_args(["synthchat", "--acp-stdio"]));
        assert!(acp_stdio_requested_from_args(["synthchat", "serve-acp"]));
        assert!(!acp_stdio_requested_from_args(["synthchat", "--dev"]));
    }

    #[test]
    fn acp_cli_action_detects_registry_entry_flags() {
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "--version"]),
            Some(AcpCliAction::Version)
        );
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "--check"]),
            Some(AcpCliAction::Check)
        );
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "--setup"]),
            Some(AcpCliAction::Setup)
        );
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "--setup-browser"]),
            Some(AcpCliAction::SetupBrowser)
        );
        assert_eq!(acp_cli_action_from_args(["synthchat", "--dev"]), None);
    }
}
