mod agent;
mod error;
mod hermes_auth;
mod llm;
mod mcp;
mod model_catalog;
mod models;
mod plugins;
mod process_utils;
mod skills;
mod store;
mod threat_patterns;
mod wechat_settings;

use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Timelike;
use error::{AppError, AppResult};
use futures::StreamExt;
use model_catalog::{DetectedModelList, ModelCapabilities, ModelCatalogEntry, ProviderCatalogInfo};
use models::{
    new_id, AgentDefinition, AppConfig, BrowserProvider, EmojiGroupConfig, ImageProvider,
    LlmProvider, Persona, ProactiveStatus, ProfileConfig, ScheduledAgentJob,
    ScheduledJobOutputRecord, SearchProvider, SendChatRequest, VideoProvider, VisionProvider,
};
use process_utils::CommandWindowExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use store::AppStore;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, DragDropEvent, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
use tokio::io::AsyncWriteExt;

const REMOTE_SKILL_FETCH_TIMEOUT_SECS: u64 = 20;
const MAX_CHAT_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;
const MAX_AVATAR_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE: usize = 64 * 1024 * 1024;
const MIN_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;
const MAX_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;
const PET_WINDOW_LABEL: &str = "pet";
const PET_WINDOW_WIDTH: f64 = 760.0;
const PET_WINDOW_HEIGHT: f64 = 560.0;
const PET_MODEL_WINDOW_WIDTH: f64 = 340.0;
const PET_MODEL_VISIBLE_HEIGHT: f64 = 440.0;
const PET_MODEL_TOP_BUFFER_HEIGHT: f64 = 96.0;
const PET_MODEL_WINDOW_HEIGHT: f64 = PET_MODEL_VISIBLE_HEIGHT + PET_MODEL_TOP_BUFFER_HEIGHT;
const PET_ORB_WINDOW_WIDTH: f64 = 84.0;
const PET_ORB_WINDOW_HEIGHT: f64 = 84.0;
const PET_DOCK_WINDOW_WIDTH: f64 = 48.0;
const PET_DOCK_WINDOW_HEIGHT: f64 = 108.0;
const PET_WINDOW_SAFE_MARGIN_TOP: i32 = 0;
const PET_WINDOW_SAFE_MARGIN_BOTTOM: i32 = 16;
const TRAY_ID: &str = "synthchat-tray";
const TRAY_OPEN_ID: &str = "open";
const TRAY_PET_ID: &str = "pet";
const TRAY_QUIT_ID: &str = "quit";
const APP_UPDATE_FETCH_TIMEOUT_SECS: u64 = 15;
const APP_UPDATE_DOWNLOAD_TIMEOUT_SECS: u64 = 600;
const MAX_APP_UPDATE_INSTALLER_BYTES: u64 = 500 * 1024 * 1024;
const APP_UPDATE_USER_AGENT: &str = "SynthChat-Update-Checker";
const DEFAULT_APP_UPDATE_MANIFEST_URL: Option<&str> = option_env!("SYNTHCHAT_UPDATE_MANIFEST_URL");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppBuildInfo {
    product_name: String,
    version: String,
    identifier: String,
    target: String,
    update_manifest_url: String,
}

#[derive(Debug, Clone)]
struct ParsedAppUpdateManifest {
    latest_version: String,
    download_url: Option<String>,
    release_url: Option<String>,
    notes: Option<String>,
    published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdateCheck {
    current_version: String,
    latest_version: String,
    update_available: bool,
    download_url: Option<String>,
    release_url: Option<String>,
    notes: Option<String>,
    published_at: Option<String>,
    source_url: String,
    checked_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdateInstallResult {
    installer_path: String,
    helper_script_path: String,
    mode: String,
    message: String,
}

#[derive(Debug, Default)]
struct PetDragState {
    active: bool,
    window_x: i32,
    window_y: i32,
    pointer_x: i32,
    pointer_y: i32,
}

#[derive(Debug, Default)]
struct PetVisionState {
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PetDockEdge {
    Left,
    Right,
}

impl PetDockEdge {
    fn from_option(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim) {
            Some("left") => Some(Self::Left),
            Some("right") => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpCliAction {
    Stdio,
    McpStdio,
    Version,
    Check,
    Setup,
    SetupBrowser,
}

pub(crate) fn state_path() -> PathBuf {
    resolve_state_path(None)
}

fn legacy_state_path_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("synthchat-data").join("state.json"));
            if let Some(grandparent) = parent.parent() {
                candidates.push(grandparent.join("synthchat-data").join("state.json"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("synthchat-data").join("state.json"));
        candidates.push(
            cwd.join("src-tauri")
                .join("target")
                .join("debug")
                .join("synthchat-data")
                .join("state.json"),
        );
        candidates.push(
            cwd.join("target")
                .join("debug")
                .join("synthchat-data")
                .join("state.json"),
        );
    }
    candidates
}

fn resolve_state_path(app: Option<&AppHandle>) -> PathBuf {
    let app_data_dir = app
        .and_then(|handle| handle.path().app_data_dir().ok())
        .or_else(|| dirs::data_dir().map(|dir| dir.join("cc.synthchat.v1")))
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
                .join("synthchat-data")
        });
    let state_dir = app_data_dir.join("synthchat-data");
    let state_path = state_dir.join("state.json");
    if !state_path.exists() {
        for candidate in legacy_state_path_candidates() {
            if candidate == state_path || !candidate.exists() {
                continue;
            }
            if let Some(parent) = state_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::copy(&candidate, &state_path);
            let candidate_dir = candidate
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let target_dir = state_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            for name in [
                "accounts.json",
                "emoji_groups.json",
                "wechat.json",
                "processes.json",
            ] {
                let source = candidate_dir.join(name);
                let target = target_dir.join(name);
                if source.exists() && !target.exists() {
                    let _ = fs::copy(source, target);
                }
            }
            break;
        }
    }
    state_path
}

fn sync_runtime_env_from_config(config: &AppConfig) {
    std::env::set_var(
        "SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY",
        config.chat.llm_credential_pool_strategy.trim(),
    );
}

fn sync_runtime_env_from_store(store: &AppStore) {
    if let Ok(config) = store.config() {
        sync_runtime_env_from_config(&config);
    }
}

fn default_app_update_manifest_url() -> String {
    normalize_app_update_manifest_url(DEFAULT_APP_UPDATE_MANIFEST_URL.unwrap_or(""))
        .unwrap_or_else(|| {
            DEFAULT_APP_UPDATE_MANIFEST_URL
                .unwrap_or("")
                .trim()
                .to_string()
        })
}

fn github_latest_manifest_download_url(owner: &str, repo: &str) -> String {
    format!("https://github.com/{owner}/{repo}/releases/latest/download/update-manifest.json")
}

fn normalize_app_update_manifest_url(raw_url: &str) -> Option<String> {
    let value = raw_url.trim();
    if value.is_empty() {
        return None;
    }
    let parsed = reqwest::Url::parse(value).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    if host == "api.github.com" {
        if segments.len() >= 6
            && segments[0] == "repos"
            && segments[3] == "releases"
            && segments[4] == "latest"
            && segments[5] == "download"
        {
            return Some(github_latest_manifest_download_url(segments[1], segments[2]));
        }
        if segments.len() >= 5
            && segments[2] == "releases"
            && segments[3] == "latest"
            && segments[4] == "download"
        {
            return Some(github_latest_manifest_download_url(segments[0], segments[1]));
        }
        if segments.len() >= 5
            && segments[0] == "repos"
            && segments[3] == "releases"
            && segments[4] == "latest"
        {
            return Some(value.to_string());
        }
        if segments.len() >= 4 && segments[2] == "releases" && segments[3] == "latest" {
            return Some(format!(
                "https://api.github.com/repos/{}/{}/releases/latest",
                segments[0], segments[1]
            ));
        }
    }
    if host == "github.com" || host == "www.github.com" {
        if segments.len() >= 4 && segments[2] == "releases" && segments[3] == "latest" {
            if segments.get(4) == Some(&"download") {
                return Some(value.to_string());
            }
            return Some(github_latest_manifest_download_url(segments[0], segments[1]));
        }
    }
    Some(value.to_string())
}

fn github_release_api_manifest_fallback(url: &reqwest::Url) -> Option<String> {
    if url.host_str()?.eq_ignore_ascii_case("api.github.com") {
        let segments: Vec<&str> = url.path_segments()?.collect();
        if segments.len() >= 5
            && segments[0] == "repos"
            && segments[3] == "releases"
            && segments[4] == "latest"
        {
            return Some(github_latest_manifest_download_url(segments[1], segments[2]));
        }
    }
    None
}

fn github_missing_update_manifest_message(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com")
    {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    if segments.len() >= 5
        && segments[2] == "releases"
        && segments[3] == "download"
        && segments
            .last()
            .map(|name| name.eq_ignore_ascii_case("update-manifest.json"))
            .unwrap_or(false)
    {
        return Some(format!(
            "GitHub Release '{}' is missing update-manifest.json. Upload release-dist/update-manifest.json as a release asset, or use the GitHub Releases API URL while staying under GitHub API rate limits.",
            segments[4]
        ));
    }
    None
}

async fn fetch_app_update_manifest_payload(
    client: &reqwest::Client,
    parsed_url: &reqwest::Url,
) -> AppResult<(Value, reqwest::Url)> {
    let response = client
        .get(parsed_url.clone())
        .header("User-Agent", APP_UPDATE_USER_AGENT)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("fetch update manifest failed: {error}")))?;
    let status = response.status();
    let final_url = response.url().clone();
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(error) => {
            if status == reqwest::StatusCode::NOT_FOUND {
                if let Some(message) = github_missing_update_manifest_message(&final_url) {
                    return Err(AppError::BadRequest(message));
                }
            }
            if matches!(
                status,
                reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::TOO_MANY_REQUESTS
            ) {
                if let Some(fallback_url) = github_release_api_manifest_fallback(parsed_url) {
                    let fallback = reqwest::Url::parse(&fallback_url).map_err(|parse_error| {
                        AppError::BadRequest(format!(
                            "invalid update manifest fallback URL: {parse_error}"
                        ))
                    })?;
                    let fallback_response = client
                        .get(fallback.clone())
                        .header("User-Agent", APP_UPDATE_USER_AGENT)
                        .send()
                        .await
                        .map_err(|fallback_error| {
                            AppError::BadRequest(format!(
                                "fetch update manifest failed: {error}; fallback failed: {fallback_error}"
                            ))
                        })?;
                    let fallback_response =
                        fallback_response.error_for_status().map_err(|fallback_error| {
                            AppError::BadRequest(format!(
                                "fetch update manifest failed: {error}; fallback failed: {fallback_error}"
                            ))
                        })?;
                    let payload = fallback_response.json::<Value>().await.map_err(|json_error| {
                        AppError::BadRequest(format!(
                            "read update manifest failed from fallback: {json_error}"
                        ))
                    })?;
                    return Ok((payload, fallback));
                }
            }
            return Err(AppError::BadRequest(format!(
                "fetch update manifest failed: {error}"
            )));
        }
    };
    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| AppError::BadRequest(format!("read update manifest failed: {error}")))?;
    Ok((payload, parsed_url.clone()))
}

fn normalize_version_text(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(&['v', 'V'][..])
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn version_part_number(part: &str) -> u64 {
    let digits: String = part.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse::<u64>().unwrap_or(0)
}

fn compare_app_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left = normalize_version_text(left);
    let right = normalize_version_text(right);
    let left_parts: Vec<&str> = left.split('.').collect();
    let right_parts: Vec<&str> = right.split('.').collect();
    let len = left_parts.len().max(right_parts.len());
    for index in 0..len {
        let left_part = left_parts
            .get(index)
            .map(|part| version_part_number(part))
            .unwrap_or(0);
        let right_part = right_parts
            .get(index)
            .map(|part| version_part_number(part))
            .unwrap_or(0);
        match left_part.cmp(&right_part) {
            std::cmp::Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    std::cmp::Ordering::Equal
}

fn json_string_field(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn update_asset_download_url(payload: &Value) -> Option<String> {
    let assets = payload
        .as_object()
        .and_then(|object| object.get("assets"))
        .and_then(Value::as_array)?;
    let mut first_url: Option<String> = None;
    let mut first_zip_url: Option<String> = None;
    for asset in assets {
        let Some(object) = asset.as_object() else {
            continue;
        };
        let Some(url) = json_string_field(
            object,
            &["browser_download_url", "downloadUrl", "download_url", "url"],
        ) else {
            continue;
        };
        if first_url.is_none() {
            first_url = Some(url.clone());
        }
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".msi") || lower.ends_with(".msix") {
            return Some(url);
        }
        if lower.ends_with(".zip") && first_zip_url.is_none() {
            first_zip_url = Some(url.clone());
        }
    }
    first_zip_url.or(first_url)
}

fn parse_app_update_manifest(payload: Value) -> AppResult<ParsedAppUpdateManifest> {
    let object = payload
        .as_object()
        .ok_or_else(|| AppError::BadRequest("update manifest must be a JSON object".into()))?;
    let latest_version = json_string_field(
        object,
        &["latestVersion", "latest_version", "version", "tag_name"],
    )
    .ok_or_else(|| {
        AppError::BadRequest("update manifest missing latestVersion/version/tag_name".into())
    })?;
    let download_url = json_string_field(
        object,
        &[
            "downloadUrl",
            "download_url",
            "installerUrl",
            "installer_url",
        ],
    )
    .or_else(|| update_asset_download_url(&payload));
    let release_url = json_string_field(object, &["releaseUrl", "release_url", "html_url"]);
    let notes = json_string_field(object, &["notes", "body", "changelog", "releaseNotes"]);
    let published_at = json_string_field(object, &["publishedAt", "published_at", "date"]);
    Ok(ParsedAppUpdateManifest {
        latest_version,
        download_url,
        release_url,
        notes,
        published_at,
    })
}

fn app_update_file_name_from_url(url: &reqwest::Url) -> AppResult<String> {
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("SynthChat-update.exe");
    let decoded = name.replace("%20", " ");
    let sanitized = sanitize_attachment_file_name(&decoded);
    let lower = sanitized.to_ascii_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".msi") || lower.ends_with(".msix") {
        return Ok(sanitized);
    }
    Err(AppError::BadRequest(
        "update asset must be an .exe, .msi, or .msix installer".into(),
    ))
}

fn app_update_installer_mode(path: &Path) -> AppResult<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("exe") => Ok("exe"),
        Some("msi") => Ok("msi"),
        Some("msix") => Ok("msix"),
        _ => Err(AppError::BadRequest(
            "update installer must end with .exe, .msi, or .msix".into(),
        )),
    }
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn write_update_helper_script(
    script_path: &Path,
    installer_path: &Path,
    mode: &str,
    current_pid: u32,
) -> AppResult<()> {
    let installer = powershell_single_quote(&installer_path.display().to_string());
    let script = format!(
        r#"$ErrorActionPreference = "Stop"
$installer = {installer}
$mode = "{mode}"
$pidToWait = {current_pid}
try {{
  Wait-Process -Id $pidToWait -Timeout 120 -ErrorAction SilentlyContinue
}} catch {{}}
Start-Sleep -Seconds 2
if ($mode -eq "msi") {{
  $process = Start-Process -FilePath "msiexec.exe" -ArgumentList @("/i", $installer, "/quiet", "/norestart") -WindowStyle Hidden -PassThru
}} elseif ($mode -eq "msix") {{
  $process = Start-Process -FilePath "powershell.exe" -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "Add-AppxPackage -ForceApplicationShutdown -Path $installer") -WindowStyle Hidden -PassThru
}} else {{
  $process = Start-Process -FilePath $installer -ArgumentList @("/S") -WindowStyle Hidden -PassThru
}}
$process.WaitForExit()
"#,
    );
    if let Some(parent) = script_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(script_path, script)?;
    Ok(())
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
        "--mcp-stdio" | "mcp-stdio" | "serve-mcp" | "--serve-mcp" => Some(AcpCliAction::McpStdio),
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
    sync_runtime_env_from_store(&store);
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
    sync_runtime_env_from_store(&store);
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

#[tauri::command(rename_all = "camelCase")]
fn get_app_build_info() -> AppBuildInfo {
    AppBuildInfo {
        product_name: "SynthChat".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        identifier: "cc.synthchat.v1".into(),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        update_manifest_url: default_app_update_manifest_url(),
    }
}

#[tauri::command(rename_all = "camelCase")]
async fn check_app_update(manifest_url: Option<String>) -> AppResult<AppUpdateCheck> {
    let manifest_url = manifest_url
        .unwrap_or_else(default_app_update_manifest_url)
        .trim()
        .to_string();
    if manifest_url.is_empty() {
        return Err(AppError::BadRequest(
            "update manifest URL is not configured".into(),
        ));
    }
    let manifest_url = normalize_app_update_manifest_url(&manifest_url).unwrap_or(manifest_url);
    let parsed_url = reqwest::Url::parse(&manifest_url)
        .map_err(|error| AppError::BadRequest(format!("invalid update manifest URL: {error}")))?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "update manifest URL must use http or https".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(APP_UPDATE_FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|error| AppError::BadRequest(format!("create update client failed: {error}")))?;
    let (payload, source_url) = fetch_app_update_manifest_payload(&client, &parsed_url).await?;
    let manifest = parse_app_update_manifest(payload)?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let update_available = compare_app_versions(&manifest.latest_version, &current_version)
        == std::cmp::Ordering::Greater;
    Ok(AppUpdateCheck {
        current_version,
        latest_version: manifest.latest_version,
        update_available,
        download_url: manifest.download_url,
        release_url: manifest.release_url,
        notes: manifest.notes,
        published_at: manifest.published_at,
        source_url: source_url.to_string(),
        checked_at: chrono::Utc::now().to_rfc3339(),
    })
}

#[tauri::command(rename_all = "camelCase")]
async fn install_app_update(
    app: AppHandle,
    download_url: String,
) -> AppResult<AppUpdateInstallResult> {
    let parsed_url = reqwest::Url::parse(download_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid update download URL: {error}")))?;
    if parsed_url.scheme() != "https" {
        return Err(AppError::BadRequest(
            "update installer download URL must use https".into(),
        ));
    }
    let file_name = app_update_file_name_from_url(&parsed_url)?;
    let updates_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| AppError::BadRequest(error.to_string()))?
        .join("updates");
    fs::create_dir_all(&updates_dir)?;
    let installer_path = updates_dir.join(file_name);
    let mode = app_update_installer_mode(&installer_path)?.to_string();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(APP_UPDATE_DOWNLOAD_TIMEOUT_SECS))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("create update downloader failed: {error}"))
        })?;
    let response = client
        .get(parsed_url.clone())
        .header("User-Agent", APP_UPDATE_USER_AGENT)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("download update installer failed: {error}"))
        })?;
    let response = response.error_for_status().map_err(|error| {
        AppError::BadRequest(format!("download update installer failed: {error}"))
    })?;
    if let Some(length) = response.content_length() {
        if length > MAX_APP_UPDATE_INSTALLER_BYTES {
            return Err(AppError::BadRequest(format!(
                "update installer is too large: {length} bytes"
            )));
        }
    }
    let mut file = tokio::fs::File::create(&installer_path)
        .await
        .map_err(AppError::Io)?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            AppError::BadRequest(format!("read update installer stream failed: {error}"))
        })?;
        downloaded += chunk.len() as u64;
        if downloaded > MAX_APP_UPDATE_INSTALLER_BYTES {
            return Err(AppError::BadRequest(format!(
                "update installer is too large: {downloaded} bytes"
            )));
        }
        file.write_all(&chunk).await.map_err(AppError::Io)?;
    }
    file.flush().await.map_err(AppError::Io)?;
    drop(file);

    let helper_script_path = updates_dir.join("install-synthchat-update.ps1");
    write_update_helper_script(
        &helper_script_path,
        &installer_path,
        &mode,
        std::process::id(),
    )?;

    #[cfg(windows)]
    {
        Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&helper_script_path)
            .hide_window()
            .spawn()?;
        app.exit(0);
    }

    #[cfg(not(windows))]
    {
        return Err(AppError::BadRequest(
            "silent update install is only supported on Windows".into(),
        ));
    }

    Ok(AppUpdateInstallResult {
        installer_path: installer_path.display().to_string(),
        helper_script_path: helper_script_path.display().to_string(),
        mode,
        message:
            "Update installer downloaded; SynthChat is closing so the installer can run silently."
                .into(),
    })
}

#[tauri::command(rename_all = "camelCase")]
fn open_app_update_url(url: String) -> AppResult<()> {
    let parsed_url = reqwest::Url::parse(url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid update URL: {error}")))?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "update URL must use http or https".into(),
        ));
    }
    let target = parsed_url.as_str();
    #[cfg(windows)]
    {
        Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", target])
            .hide_window()
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(target).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(target).spawn()?;
    }
    Ok(())
}

pub fn run_acp_stdio() -> AppResult<()> {
    let store = AppStore::new(state_path())?;
    sync_runtime_env_from_store(&store);
    let runtime = synthchat_multi_thread_runtime("synthchat-acp-worker")?;
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

pub fn run_mcp_stdio() -> AppResult<()> {
    let store = AppStore::new(state_path())?;
    sync_runtime_env_from_store(&store);
    let runtime = synthchat_multi_thread_runtime("synthchat-mcp-worker")?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                writeln!(
                    stdout,
                    "{}",
                    mcp_stdio_error_response(Value::Null, -32700, &error.to_string())
                )?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(response) = runtime.block_on(handle_mcp_stdio_json_rpc(&store, &request)) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn synthchat_tokio_worker_stack_size() -> usize {
    std::env::var("SYNTHCHAT_TOKIO_WORKER_STACK_MB")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE)
        .clamp(
            MIN_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE,
            MAX_SYNTHCHAT_TOKIO_WORKER_STACK_SIZE,
        )
}

fn synthchat_multi_thread_runtime(thread_name: &str) -> AppResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name(thread_name)
        .thread_stack_size(synthchat_tokio_worker_stack_size())
        .build()
        .map_err(AppError::Io)
}

async fn handle_mcp_stdio_json_rpc(store: &AppStore, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let Some(id) = id else {
        return None;
    };
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(mcp_stdio_error_response(
            id,
            -32600,
            "MCP request missing method",
        ));
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": mcp_stdio_protocol_version(request),
            "serverInfo": {
                "name": "synthchat-tools",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "tools": {},
                "resources": {},
                "prompts": {}
            }
        }),
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": agent::synthchat_tools_mcp_definitions()
        }),
        "resources/list" => json!({
            "resources": []
        }),
        "resources/templates/list" => json!({
            "resourceTemplates": []
        }),
        "prompts/list" => json!({
            "prompts": []
        }),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(mcp_stdio_error_response(
                    id,
                    -32602,
                    "tools/call requires params.name",
                ));
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match agent::synthchat_tools_mcp_call(store, name, arguments).await {
                Ok(text) => {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": text
                            }],
                            "isError": false
                        }
                    }));
                }
                Err(error) => {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": error.to_string()
                            }],
                            "isError": true
                        }
                    }));
                }
            }
        }
        _ => {
            return Some(mcp_stdio_error_response(
                id,
                -32601,
                &format!("MCP server method '{method}' is not supported by SynthChat yet."),
            ));
        }
    };
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    }))
}

fn mcp_stdio_protocol_version(request: &Value) -> String {
    request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("2024-11-05")
        .to_string()
}

fn mcp_stdio_error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
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
    let result = store.set_config(config.clone());
    if result.is_ok() {
        sync_runtime_env_from_config(&config);
    }
    result
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
fn add_hermes_credential_pool_entry(
    provider: String,
    label: Option<String>,
    api_key: String,
    base_url: Option<String>,
    auth_type: Option<String>,
    expires_at: Option<String>,
) -> AppResult<hermes_auth::HermesCredentialPoolEntryStatus> {
    hermes_auth::add_hermes_credential_pool_entry(
        &provider,
        label.as_deref(),
        &api_key,
        base_url.as_deref(),
        auth_type.as_deref(),
        expires_at.as_deref(),
    )
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
fn capture_screen_base64() -> AppResult<String> {
    use std::io::Cursor;

    use base64::prelude::*;
    use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};

    let monitors = xcap::Monitor::all().map_err(|e| error::AppError::BadRequest(e.to_string()))?;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| error::AppError::BadRequest("No monitors found".into()))?;
    let image = monitor
        .capture_image()
        .map_err(|e| error::AppError::BadRequest(e.to_string()))?;
    let mut image = DynamicImage::ImageRgba8(image);
    const MAX_SIDE: u32 = 1280;
    if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image = image.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle);
    }
    let rgb = image.to_rgb8();
    let mut buffer = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buffer, 86)
        .encode_image(&rgb)
        .map_err(|e| error::AppError::BadRequest(e.to_string()))?;
    let base64_str = BASE64_STANDARD.encode(buffer.into_inner());
    Ok(format!("data:image/jpeg;base64,{}", base64_str))
}

#[tauri::command(rename_all = "camelCase")]
fn set_pet_vision_active(state: State<'_, Mutex<PetVisionState>>, active: bool) -> AppResult<bool> {
    let mut guard = state
        .lock()
        .map_err(|_| AppError::BadRequest("pet vision state lock poisoned".into()))?;
    guard.active = active;
    Ok(guard.active)
}

fn pet_vision_active(app: Option<&AppHandle>) -> bool {
    let Some(app) = app else {
        return false;
    };
    app.try_state::<Mutex<PetVisionState>>()
        .and_then(|state| state.lock().ok().map(|guard| guard.active))
        .unwrap_or(false)
}

#[tauri::command(rename_all = "camelCase")]
fn get_profile(store: State<'_, AppStore>) -> AppResult<ProfileConfig> {
    store.profile()
}

#[tauri::command(rename_all = "camelCase")]
fn save_profile(store: State<'_, AppStore>, profile: ProfileConfig) -> AppResult<ProfileConfig> {
    store.set_profile(profile)
}

fn dialog_file_path_to_string(path: tauri_plugin_dialog::FilePath) -> AppResult<String> {
    path.into_path()
        .map(|path| path.to_string_lossy().to_string())
        .map_err(|error| AppError::BadRequest(format!("selected path is unavailable: {error}")))
}

fn dialog_selection_to_string(
    selected: Option<tauri_plugin_dialog::FilePath>,
) -> AppResult<Option<String>> {
    selected.map(dialog_file_path_to_string).transpose()
}

#[tauri::command(rename_all = "camelCase")]
async fn pick_path(
    window: tauri::WebviewWindow,
    title: Option<String>,
    directory: bool,
    filter_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> AppResult<Option<String>> {
    let mut dialog = window.app_handle().dialog().file().set_parent(&window);
    if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
        dialog = dialog.set_title(title);
    }
    if let Some(extensions) = extensions.filter(|items| !items.is_empty()) {
        let extension_refs = extensions.iter().map(String::as_str).collect::<Vec<_>>();
        dialog = dialog.add_filter(filter_name.unwrap_or_else(|| "Files".into()), &extension_refs);
    }

    let (tx, rx) = tokio::sync::oneshot::channel::<AppResult<Option<String>>>();
    if directory {
        dialog.pick_folder(move |selected| {
            let _ = tx.send(dialog_selection_to_string(selected));
        });
    } else {
        dialog.pick_file(move |selected| {
            let _ = tx.send(dialog_selection_to_string(selected));
        });
    }

    rx.await
        .map_err(|_| AppError::BadRequest("file dialog callback did not return a result".into()))?
}

#[tauri::command(rename_all = "camelCase")]
fn upload_profile_avatar(
    store: State<'_, AppStore>,
    file_name: String,
    bytes: Vec<u8>,
) -> AppResult<ProfileConfig> {
    validate_avatar_bytes(&bytes)?;
    let ext = image_ext_from_bytes(&bytes).unwrap_or(normalized_image_ext(&file_name)?);
    let mut profile = store.profile()?;
    if let Some(path) = &profile.avatar_path {
        remove_file_if_local(path);
    }
    let dir = store.data_dir().join("profile");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("avatar-{}.{}", new_id("profile"), ext));
    fs::write(&path, bytes)?;
    profile.avatar_path = Some(path.to_string_lossy().to_string());
    store.set_profile(profile)
}

#[tauri::command(rename_all = "camelCase")]
fn clear_profile_avatar(store: State<'_, AppStore>) -> AppResult<ProfileConfig> {
    let mut profile = store.profile()?;
    if let Some(path) = profile.avatar_path.take() {
        remove_file_if_local(&path);
    }
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
fn save_persona(
    app: AppHandle,
    store: State<'_, AppStore>,
    mut persona: Persona,
) -> AppResult<Persona> {
    persona.name = persona.name.trim().to_string();
    if persona.name.is_empty() {
        return Err(AppError::BadRequest("persona name is required".into()));
    }
    if persona.name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "persona name must be 100 characters or less".into(),
        ));
    }
    persona.id = persona.id.trim().to_string();
    if persona.id.is_empty() || persona.id.starts_with("persona-") {
        persona.id = new_id("persona");
    }
    if persona
        .id
        .chars()
        .any(|ch| matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(AppError::BadRequest(
            "persona id contains invalid characters".into(),
        ));
    }
    persona.agent_id = persona.agent_id.trim().to_string();
    persona.llm_provider = persona.llm_provider.trim().to_string();
    persona.llm_model = persona.llm_model.trim().to_string();
    persona.temperature = persona.temperature.clamp(0.0, 2.0);
    persona.max_tokens = persona.max_tokens.clamp(128, 65536);
    normalize_persona_number(&mut persona.tool_policy, "timeoutSeconds", 1.0, 86400.0);
    normalize_persona_number(&mut persona.tool_policy, "maxIterations", 1.0, 90.0);
    normalize_persona_number(&mut persona.tool_policy, "maxFailureReplans", 0.0, 32.0);
    normalize_persona_number(&mut persona.tool_policy, "retryCount", 0.0, 5.0);
    normalize_persona_number(&mut persona.tool_policy, "retryBackoffMs", 0.0, 10000.0);
    persona.emoji_send_probability = persona.emoji_send_probability.min(100);
    normalize_persona_number(&mut persona.memory, "triggerRounds", 1.0, 1000.0);
    normalize_persona_number(&mut persona.memory, "maxMemories", 1.0, 10000.0);
    normalize_persona_number(&mut persona.proactive, "minIdleHours", 0.0, 8760.0);
    normalize_persona_number(&mut persona.proactive, "maxIdleHours", 0.0, 8760.0);
    normalize_persona_number(&mut persona.proactive, "maxConsecutive", 1.0, 100.0);
    normalize_persona_number(&mut persona.voice_reply, "sampleRate", 8000.0, 48000.0);
    normalize_persona_number(&mut persona.voice_reply, "speed", 1.0, 9.0);
    normalize_persona_number(&mut persona.voice_reply, "oral", 0.0, 9.0);
    normalize_persona_number(&mut persona.voice_reply, "laugh", 0.0, 9.0);
    normalize_persona_number(&mut persona.voice_reply, "breakLevel", 0.0, 9.0);
    normalize_persona_number(&mut persona.voice_reply, "temperature", 0.01, 2.0);
    normalize_persona_number(&mut persona.voice_reply, "topP", 0.01, 1.0);
    normalize_persona_number(&mut persona.voice_reply, "topK", 1.0, 100.0);
    normalize_persona_number(&mut persona.voice_reply, "refineTemperature", 0.01, 2.0);
    normalize_persona_bool(&mut persona.voice_reply, "enabled", false);
    normalize_persona_string(&mut persona.voice_reply, "engine", "chattts");
    normalize_persona_string(&mut persona.voice_reply, "language", "zh-CN");
    normalize_persona_string(&mut persona.voice_reply, "voice", "zh-CN-XiaoxiaoNeural");
    normalize_persona_string(&mut persona.voice_reply, "volume", "+0%");
    normalize_persona_string(&mut persona.voice_reply, "pitch", "+0Hz");
    normalize_persona_string(&mut persona.voice_reply, "pythonPath", "");
    normalize_persona_string(
        &mut persona.voice_reply,
        "modelDir",
        "",
    );
    normalize_persona_string(&mut persona.voice_reply, "speakerEmbedding", "");
    normalize_persona_string(&mut persona.voice_reply, "refinePrompt", "");
    normalize_persona_bool(&mut persona.image_generation, "enabled", false);
    normalize_persona_string(&mut persona.image_generation, "provider", "");
    normalize_persona_string(&mut persona.image_generation, "model", "");
    normalize_persona_string(&mut persona.image_generation, "stylePrefix", "");
    normalize_persona_string(&mut persona.image_generation, "artStyle", "");
    normalize_persona_string(&mut persona.image_generation, "negativePrompt", "");
    normalize_persona_bool(&mut persona.image_generation, "negativeEnabled", true);
    normalize_persona_string(&mut persona.image_generation, "refMode", "avatar");
    let ref_mode = persona
        .image_generation
        .get("refMode")
        .and_then(Value::as_str)
        .unwrap_or("avatar");
    if !matches!(ref_mode, "avatar" | "custom" | "none") {
        persona.image_generation["refMode"] = json!("avatar");
    }
    let personas = store.personas()?;
    if personas
        .iter()
        .any(|item| item.id != persona.id && item.name.eq_ignore_ascii_case(&persona.name))
    {
        return Err(AppError::BadRequest("persona name already exists".into()));
    }
    if persona.avatar_path.is_none() {
        if let Some(existing) = personas.iter().find(|item| item.id == persona.id) {
            persona.avatar_path = existing.avatar_path.clone();
        }
    }
    let saved = store.save_persona(persona)?;
    let _ = app.emit(
        "synthchat-persona-event",
        serde_json::json!({
            "type": "persona_updated",
            "source": "desktop",
            "personaId": saved.id,
            "persona": saved,
        }),
    );
    Ok(saved)
}

#[tauri::command(rename_all = "camelCase")]
fn upload_persona_avatar(
    store: State<'_, AppStore>,
    persona_id: String,
    file_name: String,
    bytes: Vec<u8>,
) -> AppResult<Persona> {
    validate_avatar_bytes(&bytes)?;
    let ext = image_ext_from_bytes(&bytes).unwrap_or(normalized_image_ext(&file_name)?);
    let mut persona = store.persona(Some(&persona_id))?;
    if let Some(path) = &persona.avatar_path {
        remove_file_if_local(path);
    }
    let dir = store.data_dir().join("personas").join(&persona_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("avatar-{}.{}", new_id("persona"), ext));
    fs::write(&path, bytes)?;
    persona.avatar_path = Some(path.to_string_lossy().to_string());
    store.save_persona(persona)
}

#[tauri::command(rename_all = "camelCase")]
fn clear_persona_avatar(store: State<'_, AppStore>, persona_id: String) -> AppResult<Persona> {
    let mut persona = store.persona(Some(&persona_id))?;
    if let Some(path) = persona.avatar_path.take() {
        remove_file_if_local(&path);
    }
    store.save_persona(persona)
}

#[tauri::command(rename_all = "camelCase")]
fn list_emoji_groups(store: State<'_, AppStore>) -> AppResult<Vec<EmojiGroupConfig>> {
    ensure_default_emoji_assets(&store)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn save_emoji_groups(
    store: State<'_, AppStore>,
    mut groups: Vec<EmojiGroupConfig>,
) -> AppResult<()> {
    ensure_default_emoji_assets(&store)?;
    for group in &mut groups {
        if group.id.trim().is_empty() {
            group.id = unique_emoji_name(&store, "group")?;
        }
        group.name = group.name.trim().to_string();
        if group.name.is_empty() {
            return Err(AppError::BadRequest("emoji group name is required".into()));
        }
        let group_dir = emoji_group_dir(&store, &group.id)?;
        fs::create_dir_all(&group_dir)?;
        if group.emotions.is_empty() {
            group.emotions.push("default".into());
        }
        for emotion in &group.emotions {
            fs::create_dir_all(emoji_emotion_dir(&store, &group.id, emotion)?)?;
        }
    }
    write_emoji_groups_snapshot(&store, &groups)
}

#[tauri::command(rename_all = "camelCase")]
fn upload_emoji_image(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: Option<String>,
    file_name: String,
    bytes: Vec<u8>,
) -> AppResult<Vec<EmojiGroupConfig>> {
    const MAX_EMOJI_BYTES: usize = 10 * 1024 * 1024;
    if bytes.is_empty() || bytes.len() > MAX_EMOJI_BYTES {
        return Err(AppError::BadRequest(
            "emoji image must be between 1 byte and 10 MiB".into(),
        ));
    }
    let ext = image_ext_from_bytes(&bytes).unwrap_or(normalized_image_ext(&file_name)?);
    let group_id = validate_emoji_name(&group_id)?;
    let emotion = validate_emoji_name(
        emotion
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default"),
    )?;
    let dir = emoji_emotion_dir(&store, &group_id, &emotion)?;
    if !dir.exists() {
        return Err(AppError::NotFound(format!(
            "emoji emotion not found: {group_id}/{emotion}"
        )));
    }
    let stem = PathBuf::from(&file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_emoji_file_stem)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "emoji".into());
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut suffix = 2;
    while path.exists() {
        path = dir.join(format!("{stem}_{suffix}.{ext}"));
        suffix += 1;
    }
    fs::write(&path, bytes)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn create_emoji_group(
    store: State<'_, AppStore>,
    name: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    ensure_default_emoji_assets(&store)?;
    let name = validate_emoji_name(&name)?;
    let group = unique_emoji_name(&store, &name)?;
    fs::create_dir_all(emoji_emotion_dir(&store, &group, "default")?)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn rename_emoji_group(
    store: State<'_, AppStore>,
    group_id: String,
    new_name: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let group_id = validate_emoji_name(&group_id)?;
    let new_name = validate_emoji_name(&new_name)?;
    let src = emoji_group_dir(&store, &group_id)?;
    let dst = emoji_group_dir(&store, &new_name)?;
    if !src.is_dir() {
        return Err(AppError::NotFound(format!(
            "emoji group not found: {group_id}"
        )));
    }
    if dst.exists() {
        return Err(AppError::BadRequest(format!(
            "emoji group already exists: {new_name}"
        )));
    }
    fs::rename(src, dst)?;
    sync_persona_emoji_group(&store, &group_id, Some(&new_name))?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_emoji_group(
    store: State<'_, AppStore>,
    group_id: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let group_id = validate_emoji_name(&group_id)?;
    let dir = emoji_group_dir(&store, &group_id)?;
    if dir.is_dir() {
        fs::remove_dir_all(dir)?;
    }
    sync_persona_emoji_group(&store, &group_id, None)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn create_emoji_emotion(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let group_id = validate_emoji_name(&group_id)?;
    let emotion = validate_emoji_name(&emotion)?;
    fs::create_dir_all(emoji_emotion_dir(&store, &group_id, &emotion)?)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn rename_emoji_emotion(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: String,
    new_name: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let group_id = validate_emoji_name(&group_id)?;
    let emotion = validate_emoji_name(&emotion)?;
    let new_name = validate_emoji_name(&new_name)?;
    let src = emoji_emotion_dir(&store, &group_id, &emotion)?;
    let dst = emoji_emotion_dir(&store, &group_id, &new_name)?;
    if !src.is_dir() {
        return Err(AppError::NotFound(format!(
            "emoji emotion not found: {emotion}"
        )));
    }
    if dst.exists() {
        return Err(AppError::BadRequest(format!(
            "emoji emotion already exists: {new_name}"
        )));
    }
    fs::rename(src, dst)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_emoji_emotion(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let group_id = validate_emoji_name(&group_id)?;
    let emotion = validate_emoji_name(&emotion)?;
    let dir = emoji_emotion_dir(&store, &group_id, &emotion)?;
    if dir.is_dir() {
        fs::remove_dir_all(dir)?;
    }
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_emoji_image(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: String,
    file_name: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let path = emoji_image_path(&store, &group_id, &emotion, &file_name)?;
    if path.is_file() {
        fs::remove_file(path)?;
    }
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn rename_emoji_image(
    store: State<'_, AppStore>,
    group_id: String,
    emotion: String,
    file_name: String,
    new_name: String,
) -> AppResult<Vec<EmojiGroupConfig>> {
    let src = emoji_image_path(&store, &group_id, &emotion, &file_name)?;
    let dst = emoji_image_path(&store, &group_id, &emotion, &new_name)?;
    if !src.is_file() {
        return Err(AppError::NotFound(format!(
            "emoji image not found: {file_name}"
        )));
    }
    if dst.exists() {
        return Err(AppError::BadRequest(format!(
            "emoji image already exists: {new_name}"
        )));
    }
    fs::rename(src, dst)?;
    scan_emoji_groups(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_persona(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    if id == "default" {
        return Err(AppError::BadRequest(
            "default persona cannot be deleted".into(),
        ));
    }
    let removed = store.delete_persona(&id)?;
    if let Some(path) = removed.avatar_path {
        remove_file_if_local(&path);
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn list_accounts() -> AppResult<Vec<wechat_settings::AccountConfig>> {
    wechat_settings::list_accounts()
}

#[tauri::command(rename_all = "camelCase")]
fn save_accounts(accounts: Vec<wechat_settings::AccountConfig>) -> AppResult<()> {
    wechat_settings::save_accounts(accounts)
}

#[tauri::command(rename_all = "camelCase")]
fn get_wechat_config() -> AppResult<wechat_settings::WechatConfig> {
    wechat_settings::get_wechat_config()
}

#[tauri::command(rename_all = "camelCase")]
fn save_wechat_config(
    config: wechat_settings::WechatConfig,
) -> AppResult<wechat_settings::WechatConfig> {
    wechat_settings::save_wechat_config(config)
}

#[tauri::command(rename_all = "camelCase")]
async fn start_wechat_qr(
    base_url: Option<String>,
) -> AppResult<wechat_settings::WechatQrStartResult> {
    wechat_settings::start_wechat_qr(base_url).await
}

#[tauri::command(rename_all = "camelCase")]
async fn check_wechat_qr_status(
    qrcode: String,
    base_url: Option<String>,
) -> AppResult<wechat_settings::WechatQrStatusResult> {
    wechat_settings::check_wechat_qr_status(qrcode, base_url).await
}

#[tauri::command(rename_all = "camelCase")]
fn list_wechat_links(
    store: State<'_, AppStore>,
) -> AppResult<Vec<wechat_settings::WechatLinkSummary>> {
    wechat_settings::list_wechat_links(store.personas()?)
}

#[tauri::command(rename_all = "camelCase")]
fn link_wechat_account(
    persona_id: String,
    account_id: String,
) -> AppResult<Vec<wechat_settings::AccountConfig>> {
    wechat_settings::link_wechat_account(persona_id, account_id)
}

#[tauri::command(rename_all = "camelCase")]
fn unlink_wechat_account(persona_id: String) -> AppResult<Vec<wechat_settings::AccountConfig>> {
    wechat_settings::unlink_wechat_account(persona_id)
}

#[tauri::command(rename_all = "camelCase")]
async fn wechat_poll_once(
    app: AppHandle,
    store: State<'_, AppStore>,
    account_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<wechat_settings::WechatPollResult> {
    wechat_settings::wechat_poll_once(&store, &app, account_id, timeout_seconds).await
}

#[tauri::command(rename_all = "camelCase")]
async fn wechat_inbound_text(
    app: AppHandle,
    store: State<'_, AppStore>,
    account_id: String,
    user_id: String,
    text: String,
    context_token: Option<String>,
    raw_message: Option<serde_json::Value>,
    attachments: Option<Vec<serde_json::Value>>,
) -> AppResult<wechat_settings::WechatInboundResult> {
    wechat_settings::wechat_inbound_text_with_extras(
        &store,
        &app,
        account_id,
        user_id,
        text,
        context_token,
        wechat_settings::WechatInboundExtras {
            raw_message,
            attachments: attachments.unwrap_or_default(),
        },
    )
    .await
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
fn delete_conversation(
    store: State<'_, AppStore>,
    id: String,
) -> AppResult<agent::ConversationDeleteMemorySettlingResult> {
    let settling_plan = match agent::snapshot_conversation_memory_before_delete(&store, &id) {
        Ok(plan) => plan,
        Err(error) => {
            store.delete_conversation(&id)?;
            return Ok(agent::ConversationDeleteMemorySettlingResult {
                status: "failed".into(),
                reason: Some(error.to_string()),
                memory_count: 0,
            });
        }
    };
    store.delete_conversation(&id)?;
    match settling_plan {
        agent::ConversationMemorySettlingPlan::Skip(result) => Ok(result),
        agent::ConversationMemorySettlingPlan::Schedule(snapshot) => {
            let store = store.inner().clone();
            tauri::async_runtime::spawn(async move {
                let result = agent::settle_conversation_memory_snapshot(&store, snapshot).await;
                if result.status == "failed" {
                    eprintln!(
                        "SynthChat background memory settling after conversation delete failed: {}",
                        result.reason.unwrap_or_else(|| "unknown error".into())
                    );
                }
            });
            Ok(agent::ConversationDeleteMemorySettlingResult {
                status: "scheduled".into(),
                reason: Some("background memory settling scheduled".into()),
                memory_count: 0,
            })
        }
    }
}

#[tauri::command(rename_all = "camelCase")]
fn rename_conversation(store: State<'_, AppStore>, id: String, title: String) -> AppResult<()> {
    store.rename_conversation(&id, title)
}

#[tauri::command(rename_all = "camelCase")]
fn set_conversation_agent(
    store: State<'_, AppStore>,
    id: String,
    agent_id: String,
) -> AppResult<models::Conversation> {
    store.set_conversation_agent(&id, agent_id)
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
    let mut messages = agent::run_chat_turn(&store, request, Some(&app)).await?;
    let assistant_index = messages
        .iter()
        .rev()
        .position(|message| message.role == "assistant")
        .map(|reverse_index| messages.len() - 1 - reverse_index);
    if let Some(index) = assistant_index {
        let conversation_id = messages[index].conversation_id.clone();
        if let Ok(conversation) = store.conversation(&conversation_id) {
            if let Ok(persona) = store.persona(conversation.persona_id.as_deref()) {
                let resolved =
                    apply_persona_emoji(&store, &persona, messages[index].content.clone());
                if resolved != messages[index].content {
                    let saved = store.update_message_content(
                        &conversation_id,
                        &messages[index].id,
                        resolved,
                    )?;
                    messages[index] = saved;
                }
            }
            let assistant = &messages[index];
            wechat_settings::dispatch_desktop_reply_to_wechat(
                &store,
                &conversation,
                &assistant.content,
            );
        }
    }
    Ok(messages)
}

#[tauri::command(rename_all = "camelCase")]
fn delete_message(_store: State<'_, AppStore>, _message_id: String) -> AppResult<()> {
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn list_proactive_statuses(
    app: AppHandle,
    store: State<'_, AppStore>,
) -> AppResult<Vec<ProactiveStatus>> {
    let suspended = pet_vision_active(Some(&app));
    store
        .personas()?
        .iter()
        .map(|persona| proactive_status_for_persona_with_runtime(&store, persona, suspended))
        .collect()
}

#[tauri::command(rename_all = "camelCase")]
async fn trigger_proactive_once(
    app: AppHandle,
    store: State<'_, AppStore>,
    persona_id: String,
) -> AppResult<ProactiveStatus> {
    let persona = store.persona(Some(&persona_id))?;
    Box::pin(trigger_proactive_for_persona(&app, &store, &persona, true)).await?;
    proactive_status_for_persona(&store, &persona)
}

async fn trigger_proactive_for_persona(
    app: &AppHandle,
    store: &AppStore,
    persona: &Persona,
    force: bool,
) -> AppResult<bool> {
    let status = proactive_status_for_persona(&store, &persona)?;
    if status.conversation_busy {
        return Ok(false);
    }
    if !force && !status.can_fire {
        return Ok(false);
    }
    let conversation_id = status
        .conversation_id
        .clone()
        .ok_or_else(|| AppError::BadRequest("没有该角色的会话，无法发送主动消息".into()))?;
    let prompt = persona
        .proactive
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("用户已经一段时间没有回复了。请根据角色设定与近期对话，主动发起一条贴合角色的简短消息。")
        .to_string();
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "processing",
            "source": "proactive",
            "personaId": persona.id,
            "conversationId": conversation_id,
        }),
    );
    let request = SendChatRequest {
        conversation_id: Some(conversation_id.clone()),
        persona_id: Some(persona.id.clone()),
        agent_id: None,
        content: prompt,
        provider_data: Some(json!({"source": "proactive-internal", "silent": true})),
        queue_item_id: None,
    };
    let generated = match agent::run_chat_turn(store, request, Some(app)).await {
        Ok(messages) => messages,
        Err(error) => {
            let _ = app.emit(
                "synthchat-chat-event",
                json!({
                    "type": "conversation_updated",
                    "source": "proactive",
                    "personaId": persona.id,
                    "conversationId": conversation_id,
                }),
            );
            if let Err(drain_error) =
                agent::drain_queued_requests_for_conversation(store, &conversation_id, Some(app))
                    .await
            {
                eprintln!(
                    "SynthChat proactive error queue drain failed: conversation={} error={}",
                    conversation_id, drain_error
                );
            }
            return Err(error);
        }
    };
    let internal_user_ids = generated
        .iter()
        .filter(|message| message.role == "user" && message.source == "proactive-internal")
        .map(|message| message.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let assistant_message_ids = generated
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let messages = store.finalize_proactive_messages(
        &conversation_id,
        &assistant_message_ids,
        &internal_user_ids,
    )?;
    if let Some(assistant) = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && message.source == "proactive")
    {
        if let Ok(conversation) = store.conversation(&conversation_id) {
            wechat_settings::dispatch_desktop_reply_to_wechat(
                store,
                &conversation,
                &assistant.content,
            );
        }
    }
    if let Err(error) =
        agent::drain_queued_requests_for_conversation(store, &conversation_id, Some(app)).await
    {
        eprintln!(
            "SynthChat proactive queue drain failed: conversation={} error={}",
            conversation_id, error
        );
    }
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "conversation_updated",
            "source": "proactive",
            "personaId": persona.id,
            "conversationId": conversation_id,
        }),
    );
    Ok(true)
}

async fn run_proactive_loop(app: AppHandle, store: AppStore) {
    let interval_seconds = std::env::var("SYNTHCHAT_PROACTIVE_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(5, 3600);
    let mut next_fire_at = HashMap::<String, i64>::new();
    loop {
        tokio::time::sleep(Duration::from_secs(interval_seconds)).await;
        let Ok(personas) = store.personas() else {
            continue;
        };
        let now = epoch_seconds_now();
        for persona in personas {
            if pet_vision_active(Some(&app)) {
                continue;
            }
            if !persona
                .proactive
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            if next_fire_at
                .get(&persona.id)
                .is_some_and(|scheduled| *scheduled > now)
            {
                continue;
            }
            if let Err(error) =
                Box::pin(trigger_proactive_for_persona(&app, &store, &persona, false)).await
            {
                eprintln!("SynthChat proactive failed: {error}");
            } else if let Ok(status) = proactive_status_for_persona(&store, &persona) {
                next_fire_at.insert(persona.id.clone(), now + status.wait_seconds as i64);
            }
        }
    }
}

fn proactive_status_for_persona(store: &AppStore, persona: &Persona) -> AppResult<ProactiveStatus> {
    proactive_status_for_persona_with_runtime(store, persona, false)
}

fn proactive_status_for_persona_with_runtime(
    store: &AppStore,
    persona: &Persona,
    pet_vision_suspended: bool,
) -> AppResult<ProactiveStatus> {
    let conversation = store
        .conversations()?
        .into_iter()
        .filter(|conversation| conversation.persona_id.as_deref() == Some(persona.id.as_str()))
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at));
    let messages = if let Some(conversation) = &conversation {
        store.messages(&conversation.id, None)?
    } else {
        Vec::new()
    };
    let conversation_busy = conversation.as_ref().is_some_and(|conversation| {
        store
            .active_agent_run_for_conversation(&conversation.id)
            .ok()
            .flatten()
            .is_some()
    });
    let last_user_at = messages
        .iter()
        .rev()
        .find(|message| proactive_message_counts_as_user_activity(message))
        .and_then(|message| epoch_seconds_from_iso(&message.created_at))
        .unwrap_or(0);
    let last_user_index = messages
        .iter()
        .rposition(proactive_message_counts_as_user_activity);
    let last_reply_at = last_user_index
        .and_then(|index| {
            messages[index + 1..]
                .iter()
                .rev()
                .find(|message| proactive_message_counts_as_reply_anchor(message))
        })
        .and_then(|message| epoch_seconds_from_iso(&message.created_at))
        .unwrap_or(0);
    let consecutive_count = messages
        .iter()
        .rev()
        .take_while(|message| !proactive_message_counts_as_user_activity(message))
        .filter(|message| message.role == "assistant" && message.source == "proactive")
        .count() as u32;
    let wait_seconds = proactive_wait_seconds(&persona.id, &persona.proactive);
    let now = epoch_seconds_now();
    let seconds_since_last_user = if last_user_at > 0 {
        now.saturating_sub(last_user_at)
    } else {
        0
    };
    let seconds_since_last_reply = if last_reply_at > 0 {
        now.saturating_sub(last_reply_at)
    } else {
        0
    };
    let in_quiet_hours = proactive_in_quiet_hours(&persona.proactive);
    let ready_in_seconds = wait_seconds as i64 - seconds_since_last_reply;
    let enabled = persona
        .proactive
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_consecutive = persona
        .proactive
        .get("maxConsecutive")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 100) as u32;
    let mut blocked_reason = String::new();
    if !enabled {
        blocked_reason = "主动消息未启用".into();
    } else if pet_vision_suspended {
        blocked_reason = "视觉感知运行中，主动消息已暂停".into();
    } else if conversation.is_none() {
        blocked_reason = "没有该角色的会话".into();
    } else if last_user_at <= 0 {
        blocked_reason = "没有历史用户消息，无法锚定空闲时间".into();
    } else if last_reply_at <= 0 {
        blocked_reason = "等待助手回复完成".into();
    } else if in_quiet_hours {
        blocked_reason = "当前处于静默时段".into();
    } else if consecutive_count >= max_consecutive {
        blocked_reason = "已达到用户回复前的连续主动消息上限".into();
    } else if ready_in_seconds > 0 {
        blocked_reason = format!("还需等待 {} 秒", ready_in_seconds);
    } else if conversation_busy {
        blocked_reason = "当前会话正在处理其他请求".into();
    } else if let Some(conversation) = &conversation {
        if conversation.wechat_account_id.is_some() && seconds_since_last_user > 82_800 {
            blocked_reason = "微信上下文超过 23 小时安全窗口".into();
        }
    }
    Ok(ProactiveStatus {
        persona_id: persona.id.clone(),
        persona_name: persona.name.clone(),
        enabled,
        conversation_id: conversation.map(|conversation| conversation.id),
        conversation_busy,
        last_user_at,
        seconds_since_last_user,
        last_reply_at,
        seconds_since_last_reply,
        wait_seconds,
        ready_in_seconds: ready_in_seconds.max(0),
        consecutive_count,
        max_consecutive,
        in_quiet_hours,
        pet_vision_suspended,
        can_fire: blocked_reason.is_empty(),
        blocked_reason,
    })
}

fn proactive_message_counts_as_user_activity(message: &models::ChatMessage) -> bool {
    message.role == "user" && message.source != "proactive-internal"
}

fn proactive_message_counts_as_reply_anchor(message: &models::ChatMessage) -> bool {
    message.role == "assistant"
        && message.source != "desktop-agent-error"
        && message.source != "proactive-internal"
}

fn proactive_wait_seconds(persona_id: &str, config: &Value) -> u64 {
    let min = config
        .get("minIdleHours")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .max(0.0);
    let max = config
        .get("maxIdleHours")
        .and_then(Value::as_f64)
        .unwrap_or(3.0)
        .max(min)
        .max(0.0);
    let min_seconds = (min * 3600.0).round() as u64;
    let max_seconds = (max * 3600.0).round() as u64;
    if max_seconds <= min_seconds {
        return min_seconds;
    }
    let salt = persona_id
        .bytes()
        .fold(epoch_seconds_now().unsigned_abs(), |acc, value| {
            acc + value as u64
        });
    min_seconds + salt % (max_seconds - min_seconds + 1)
}

fn proactive_in_quiet_hours(config: &Value) -> bool {
    let quiet = config.get("quietHours").unwrap_or(&Value::Null);
    if !quiet
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return false;
    }
    let start = quiet
        .get("start")
        .and_then(Value::as_str)
        .and_then(parse_hhmm_minutes);
    let end = quiet
        .get("end")
        .and_then(Value::as_str)
        .and_then(parse_hhmm_minutes);
    let (Some(start), Some(end)) = (start, end) else {
        return false;
    };
    let now = chrono::Local::now();
    let current = now.hour() as u32 * 60 + now.minute();
    if start <= end {
        current >= start && current <= end
    } else {
        current >= start || current <= end
    }
}

fn parse_hhmm_minutes(value: &str) -> Option<u32> {
    let mut parts = value.trim().split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

fn epoch_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_seconds_from_iso(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.timestamp())
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
async fn detect_provider_models(provider: LlmProvider) -> AppResult<DetectedModelList> {
    model_catalog::detect_provider_models(provider).await
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
fn get_mcp_status(store: State<'_, AppStore>) -> AppResult<Value> {
    mcp::mcp_status(&store)
}

#[tauri::command(rename_all = "camelCase")]
async fn reset_mcp_persistent_session(
    store: State<'_, AppStore>,
    server_id: Option<String>,
) -> AppResult<Value> {
    mcp::reset_mcp_persistent_session(&store, server_id.as_deref()).await
}

#[tauri::command(rename_all = "camelCase")]
fn remove_mcp_oauth_tokens(store: State<'_, AppStore>, server_id: String) -> AppResult<Value> {
    mcp::remove_mcp_oauth_tokens(&store, &server_id)
}

#[tauri::command(rename_all = "camelCase")]
async fn refresh_mcp_oauth_tokens(
    store: State<'_, AppStore>,
    server_id: String,
) -> AppResult<Value> {
    mcp::refresh_mcp_oauth_tokens(&store, &server_id).await
}

#[tauri::command(rename_all = "camelCase")]
async fn start_mcp_oauth_login(store: State<'_, AppStore>, server_id: String) -> AppResult<Value> {
    mcp::start_mcp_oauth_login(&store, &server_id).await
}

#[tauri::command(rename_all = "camelCase")]
async fn finish_mcp_oauth_login(
    store: State<'_, AppStore>,
    server_id: String,
    code_or_callback_url: String,
) -> AppResult<Value> {
    mcp::finish_mcp_oauth_login(&store, &server_id, &code_or_callback_url).await
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
        None,
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
fn list_agent_runtime_events(
    store: State<'_, AppStore>,
    conversation_id: Option<String>,
    run_id: Option<String>,
    queue_item_id: Option<String>,
    task_id: Option<String>,
    board: Option<String>,
    since: Option<u64>,
    limit: Option<u64>,
) -> AppResult<Value> {
    store.reload_from_disk()?;
    agent::agent_runtime_events(
        &store,
        &serde_json::json!({
            "action": "kanban-runtime-events",
            "conversationId": conversation_id,
            "runId": run_id,
            "queueItemId": queue_item_id,
            "taskId": task_id,
            "board": board,
            "since": since.unwrap_or(0),
            "limit": limit.unwrap_or(80),
        }),
    )
}

#[tauri::command(rename_all = "camelCase")]
fn list_managed_processes(store: State<'_, AppStore>) -> AppResult<Vec<Value>> {
    store.managed_processes()
}

#[tauri::command(rename_all = "camelCase")]
fn stop_managed_process(
    store: State<'_, AppStore>,
    process_id: String,
    forget: Option<bool>,
) -> AppResult<Value> {
    store.stop_managed_process(&process_id, forget.unwrap_or(false))
}

#[tauri::command(rename_all = "camelCase")]
async fn browser_runtime_status(store: State<'_, AppStore>) -> AppResult<Value> {
    agent::browser_runtime_status(&store).await
}

#[tauri::command(rename_all = "camelCase")]
async fn computer_use_runtime_status(store: State<'_, AppStore>) -> AppResult<Value> {
    agent::computer_use_runtime_status(&store).await
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_control_commands() -> Vec<agent::AgentControlCommandView> {
    agent::list_agent_control_commands()
}

#[tauri::command(rename_all = "camelCase")]
fn list_plugin_auxiliary_tasks(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::PluginAuxiliaryTaskSummary>> {
    agent::list_python_plugin_auxiliary_tasks(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_auxiliary_tasks(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::AgentAuxiliaryTaskSummary>> {
    agent::list_agent_auxiliary_tasks(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn agent_auxiliary_task_defaults(
    store: State<'_, AppStore>,
    key: String,
) -> AppResult<serde_json::Value> {
    agent::agent_auxiliary_task_defaults(&store, &key)
}

#[tauri::command(rename_all = "camelCase")]
fn list_agent_auxiliary_task_assignments(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::AgentAuxiliaryTaskAssignment>> {
    agent::list_agent_auxiliary_task_assignments(&store)
}

#[tauri::command(rename_all = "camelCase")]
fn save_agent_auxiliary_task_assignment(
    store: State<'_, AppStore>,
    key: String,
    provider: String,
    model: String,
    base_url: String,
    api_key: String,
    timeout: Option<u64>,
    extra_body: Option<serde_json::Value>,
) -> AppResult<Vec<models::AgentAuxiliaryTaskAssignment>> {
    agent::save_agent_auxiliary_task_assignment(
        &store, &key, &provider, &model, &base_url, &api_key, timeout, extra_body,
    )
}

#[tauri::command(rename_all = "camelCase")]
fn reset_agent_auxiliary_task_assignments(
    store: State<'_, AppStore>,
) -> AppResult<Vec<models::AgentAuxiliaryTaskAssignment>> {
    agent::reset_agent_auxiliary_task_assignments(&store)
}

#[tauri::command(rename_all = "camelCase")]
async fn judge_agent_goal(
    store: State<'_, AppStore>,
    goal: String,
    response: String,
    subgoals: Option<Vec<String>>,
) -> AppResult<Value> {
    agent::judge_agent_goal(&store, &goal, &response, subgoals.unwrap_or_default()).await
}

#[tauri::command(rename_all = "camelCase")]
fn agent_goal_status(store: State<'_, AppStore>, conversation_id: String) -> AppResult<Value> {
    agent::agent_goal_status(&store, &conversation_id)
}

#[tauri::command(rename_all = "camelCase")]
fn set_agent_goal(
    store: State<'_, AppStore>,
    conversation_id: String,
    goal: String,
    max_turns: Option<u32>,
) -> AppResult<Value> {
    agent::set_agent_goal(&store, &conversation_id, &goal, max_turns)
}

#[tauri::command(rename_all = "camelCase")]
fn pause_agent_goal(
    store: State<'_, AppStore>,
    conversation_id: String,
    reason: Option<String>,
) -> AppResult<Value> {
    agent::pause_agent_goal(&store, &conversation_id, reason.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
fn resume_agent_goal(
    store: State<'_, AppStore>,
    conversation_id: String,
    reset_budget: Option<bool>,
) -> AppResult<Value> {
    agent::resume_agent_goal(&store, &conversation_id, reset_budget.unwrap_or(true))
}

#[tauri::command(rename_all = "camelCase")]
fn clear_agent_goal(store: State<'_, AppStore>, conversation_id: String) -> AppResult<Value> {
    agent::clear_agent_goal(&store, &conversation_id)
}

#[tauri::command(rename_all = "camelCase")]
fn add_agent_subgoal(
    store: State<'_, AppStore>,
    conversation_id: String,
    text: String,
) -> AppResult<Value> {
    agent::add_agent_subgoal(&store, &conversation_id, &text)
}

#[tauri::command(rename_all = "camelCase")]
fn remove_agent_subgoal(
    store: State<'_, AppStore>,
    conversation_id: String,
    index: usize,
) -> AppResult<Value> {
    agent::remove_agent_subgoal(&store, &conversation_id, index)
}

#[tauri::command(rename_all = "camelCase")]
fn clear_agent_subgoals(store: State<'_, AppStore>, conversation_id: String) -> AppResult<Value> {
    agent::clear_agent_subgoals(&store, &conversation_id)
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
async fn dispatch_kanban_and_drain_agent_queue(
    app: AppHandle,
    store: State<'_, AppStore>,
    payload: serde_json::Value,
) -> AppResult<Value> {
    agent::dispatch_kanban_and_drain_agent_queue(&store, Some(&app), payload).await
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
fn save_agent(
    store: State<'_, AppStore>,
    mut agent: AgentDefinition,
) -> AppResult<AgentDefinition> {
    agent.name = agent.name.trim().to_string();
    if agent.name.is_empty() {
        return Err(AppError::BadRequest("agent name is required".into()));
    }
    if agent.name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "agent name must be 100 characters or less".into(),
        ));
    }
    agent.id = agent.id.trim().to_string();
    if agent.id.is_empty() {
        agent.id = new_id("agent");
    }
    if agent
        .id
        .chars()
        .any(|ch| matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(AppError::BadRequest(
            "agent id contains invalid characters".into(),
        ));
    }
    agent.description = agent.description.trim().to_string();
    agent.workspace_dir = agent.workspace_dir.trim().to_string();
    agent.llm_provider = agent.llm_provider.trim().to_string();
    agent.llm_model = agent.llm_model.trim().to_string();
    agent.skills_dir = agent.skills_dir.trim().to_string();
    agent.max_subagents = agent.max_subagents.clamp(1, 32);
    agent.max_subagent_depth = agent.max_subagent_depth.clamp(1, 4);
    agent.max_tool_iterations = agent.max_tool_iterations.clamp(1, 90);
    let agents = store.agents()?;
    if agents
        .iter()
        .any(|item| item.id != agent.id && item.name.eq_ignore_ascii_case(&agent.name))
    {
        return Err(AppError::BadRequest("agent name already exists".into()));
    }
    store.save_agent(agent)
}

#[tauri::command(rename_all = "camelCase")]
async fn auto_describe_agent(
    store: State<'_, AppStore>,
    agent_id: Option<String>,
    overwrite: Option<bool>,
) -> AppResult<AgentDefinition> {
    agent::auto_describe_agent(&store, agent_id, overwrite.unwrap_or(false)).await
}

#[tauri::command(rename_all = "camelCase")]
fn delete_agent(store: State<'_, AppStore>, id: String) -> AppResult<()> {
    let clean = id.trim();
    if clean.is_empty() {
        return Err(AppError::BadRequest("agent id is required".into()));
    }
    store.delete_agent(clean)
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
        agent.max_subagents = value.clamp(1, 32) as u32;
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
    let normalized_path = path.trim_matches('/');
    let url = if normalized_path.is_empty() {
        format!("https://api.github.com/repos/{repo}/contents")
    } else {
        format!(
            "https://api.github.com/repos/{repo}/contents/{}",
            normalized_path.replace(' ', "%20")
        )
    };
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
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|error| AppError::BadRequest(format!("invalid skill url: {error}")))?;
    let client = skill_http_client()?;
    if let Some(raw_url) = github_blob_skill_raw_url(&parsed) {
        let raw = fetch_text_url(
            &client,
            &raw_url,
            "fetch GitHub skill content",
            "read GitHub skill content",
        )
        .await?;
        if looks_like_html_document(&raw) {
            return Err(AppError::BadRequest(
                "GitHub skill URL resolved to an HTML page; please use a blob/tree URL that points to SKILL.md or its containing directory".into(),
            ));
        }
        return Ok(raw);
    }
    let raw = fetch_text_url(&client, trimmed, "fetch skill url", "read skill url").await?;
    if is_skills_sh_host(&parsed) {
        return resolve_skills_sh_skill_url(&client, &parsed, &raw).await;
    }
    if looks_like_html_document(&raw) {
        return Err(AppError::BadRequest(
            "public url returned an HTML page; please provide a direct SKILL.md URL or a supported skills.sh skill page URL".into(),
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

async fn fetch_text_url(
    client: &reqwest::Client,
    url: &str,
    fetch_context: &str,
    read_context: &str,
) -> AppResult<String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{fetch_context} failed: {error}")))?;
    if let Some(length) = response.content_length() {
        if length > 512 * 1024 {
            return Err(AppError::BadRequest(
                "skill url response is too large".into(),
            ));
        }
    }
    let response = response
        .error_for_status()
        .map_err(|error| AppError::BadRequest(format!("{fetch_context} failed: {error}")))?;
    let raw = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("{read_context} failed: {error}")))?;
    if raw.len() > 512 * 1024 {
        return Err(AppError::BadRequest(
            "skill url response is too large".into(),
        ));
    }
    Ok(raw)
}

fn looks_like_html_document(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
}

fn is_skills_sh_host(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str().map(|host| host.to_ascii_lowercase()),
        Some(host) if host == "skills.sh" || host == "www.skills.sh"
    )
}

fn is_github_host(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str().map(|host| host.to_ascii_lowercase()),
        Some(host) if host == "github.com" || host == "www.github.com"
    )
}

fn github_blob_skill_raw_url(url: &reqwest::Url) -> Option<String> {
    if !is_github_host(url) {
        return None;
    }
    let segments = url
        .path_segments()
        .map(|items| {
            items
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let owner = *segments.first()?;
    let repo = *segments.get(1)?;
    let mode = *segments.get(2)?;
    if owner.is_empty() || repo.is_empty() || !matches!(mode, "blob" | "tree") {
        return None;
    }
    let branch = *segments.get(3)?;
    let mut skill_path = segments.get(4..)?.join("/");
    if skill_path.is_empty() {
        return None;
    }
    if mode == "tree" && !skill_path.to_ascii_lowercase().ends_with("/skill.md") {
        skill_path = format!("{}/SKILL.md", skill_path.trim_end_matches('/'));
    }
    if !skill_path.to_ascii_lowercase().ends_with("/skill.md")
        && !skill_path.eq_ignore_ascii_case("SKILL.md")
    {
        return None;
    }
    Some(format!(
        "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
        skill_path.trim_start_matches('/')
    ))
}

fn normalize_skill_lookup_token(value: &str) -> String {
    let mut output = String::new();
    let mut last_was_dash = false;
    for ch in value.chars() {
        let normalized = ch.to_ascii_lowercase();
        if normalized.is_ascii_alphanumeric() {
            output.push(normalized);
            last_was_dash = false;
        } else if !last_was_dash {
            output.push('-');
            last_was_dash = true;
        }
    }
    output.trim_matches('-').to_string()
}

fn parse_skills_sh_install_hint(html: &str) -> Option<(String, String)> {
    let marker = "npx skills add ";
    let start = html.find(marker)?;
    let rest = &html[start + marker.len()..];
    let split = rest.find(" --skill ")?;
    let repo_url = rest[..split].trim();
    let skill_rest = &rest[split + " --skill ".len()..];
    let skill = skill_rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.'))
        .collect::<String>()
        .trim()
        .to_string();
    if repo_url.starts_with("https://github.com/") && !skill.is_empty() {
        Some((repo_url.to_string(), skill))
    } else {
        None
    }
}

fn parse_skills_sh_path(url: &reqwest::Url) -> Option<(String, String)> {
    let segments = url
        .path_segments()
        .map(|items| {
            items
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if segments.len() < 3 {
        return None;
    }
    let owner = segments.first()?;
    let repo = segments.get(1)?;
    let skill = segments.last()?;
    Some((
        format!("https://github.com/{owner}/{repo}"),
        (*skill).to_string(),
    ))
}

fn parse_github_repo_identifier(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let path = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))?;
    let mut parts = path.split('/').filter(|value| !value.trim().is_empty());
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn extract_frontmatter_name(raw: &str) -> Option<String> {
    let mut lines = raw.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            let cleaned = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

fn extract_markdown_heading(raw: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn skill_raw_matches_slug(raw: &str, target_slug: &str) -> bool {
    extract_frontmatter_name(raw)
        .into_iter()
        .chain(extract_markdown_heading(raw))
        .map(|value| normalize_skill_lookup_token(&value))
        .any(|value| value == target_slug)
}

async fn resolve_skills_sh_skill_url(
    client: &reqwest::Client,
    url: &reqwest::Url,
    html: &str,
) -> AppResult<String> {
    let path_segments = url
        .path_segments()
        .map(|items| {
            items
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if path_segments.len() < 3 {
        return Err(AppError::BadRequest(
            "skills.sh 首页或目录页不能直接安装；请使用具体技能详情页 URL，例如 https://www.skills.sh/vercel-labs/skills/find-skills".into(),
        ));
    }
    let (repo_url, skill_slug) = parse_skills_sh_install_hint(html)
        .or_else(|| parse_skills_sh_path(url))
        .ok_or_else(|| {
            AppError::BadRequest(
                "无法从 skills.sh 页面解析技能来源；请改用具体技能详情页或直接粘贴 SKILL.md 原始链接".into(),
            )
        })?;
    let repo = parse_github_repo_identifier(&repo_url).ok_or_else(|| {
        AppError::BadRequest(format!(
            "无法解析 skills.sh 页面中的 GitHub 仓库地址: {repo_url}"
        ))
    })?;
    fetch_github_skill_by_slug(client, &repo, &skill_slug).await
}

async fn fetch_github_skill_by_slug(
    client: &reqwest::Client,
    repo: &str,
    skill_slug: &str,
) -> AppResult<String> {
    let target_slug = normalize_skill_lookup_token(skill_slug);
    if target_slug.is_empty() {
        return Err(AppError::BadRequest("invalid remote skill slug".into()));
    }
    for root in ["skills", ""] {
        if let Some(raw) = search_github_skill_under_path(client, repo, root, &target_slug).await? {
            return Ok(raw);
        }
    }
    Err(AppError::BadRequest(format!(
        "无法在 GitHub 仓库 {repo} 中定位 skills.sh 技能 `{skill_slug}`；请改用该技能的原始 SKILL.md 链接"
    )))
}

async fn search_github_skill_under_path(
    client: &reqwest::Client,
    repo: &str,
    root: &str,
    target_slug: &str,
) -> AppResult<Option<String>> {
    let mut stack = vec![(root.trim_matches('/').to_string(), 0usize)];
    let mut visited_entries = 0usize;
    while let Some((path, depth)) = stack.pop() {
        if visited_entries >= 160 || depth > 4 {
            continue;
        }
        let entries = match fetch_github_contents(client, repo, &path).await {
            Ok(items) => items,
            Err(_) if depth == 0 => continue,
            Err(_) => continue,
        };
        visited_entries += entries.len();
        for entry in entries {
            if entry.entry_type == "dir" {
                if depth < 4 {
                    stack.push((entry.path, depth + 1));
                }
                continue;
            }
            if entry.entry_type != "file" || !entry.name.eq_ignore_ascii_case("SKILL.md") {
                continue;
            }
            let Some(download_url) = entry.download_url.as_deref() else {
                continue;
            };
            let folder_slug = entry
                .path
                .trim_end_matches("/SKILL.md")
                .rsplit('/')
                .next()
                .map(normalize_skill_lookup_token)
                .unwrap_or_default();
            if folder_slug != target_slug && !entry.path.to_ascii_lowercase().contains(target_slug)
            {
                continue;
            }
            let raw = fetch_text_url(
                client,
                download_url,
                "fetch GitHub skill content",
                "read GitHub skill content",
            )
            .await?;
            if folder_slug == target_slug || skill_raw_matches_slug(&raw, target_slug) {
                return Ok(Some(raw));
            }
        }
    }
    Ok(None)
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
    let prompt_memories = memories
        .iter()
        .filter(|memory| matches!(memory.target.as_str(), "memory" | "user"))
        .collect::<Vec<_>>();
    let prompt_safe = memories
        .iter()
        .filter(|memory| matches!(memory.target.as_str(), "memory" | "user"))
        .filter(|memory| store::scan_memory_content(&memory.summary).is_none())
        .count();
    let blocked_by_security_scan = prompt_memories.len().saturating_sub(prompt_safe);
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
        total: prompt_memories.len(),
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
        target: memory
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("memory")
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
async fn transcribe_chat_audio(
    store: State<'_, AppStore>,
    data_url: String,
    mime_type: Option<String>,
) -> AppResult<Value> {
    let mut payload = json!({
        "dataUrl": data_url,
    });
    if let Some(mime_type) = mime_type
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["mimeType"] = json!(mime_type);
    }
    agent::transcribe_audio_payload_for_desktop(&store, &payload).await
}

#[tauri::command(rename_all = "camelCase")]
async fn speak_chat_text(
    store: State<'_, AppStore>,
    text: String,
    provider_id: Option<String>,
    language: Option<String>,
    voice: Option<String>,
    volume: Option<String>,
    pitch: Option<String>,
    format: Option<String>,
    engine: Option<String>,
    speed_scale: Option<String>,
    speed: Option<f64>,
    model_dir: Option<String>,
    python_path: Option<String>,
    sample_rate: Option<u32>,
    oral: Option<u32>,
    laugh: Option<u32>,
    break_level: Option<u32>,
    speaker_seed: Option<u64>,
    speaker_embedding: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u32>,
    refine_text_enabled: Option<bool>,
    refine_prompt: Option<String>,
    refine_temperature: Option<f64>,
) -> AppResult<Value> {
    let mut payload = json!({
        "text": text,
        "format": format.unwrap_or_else(|| "mp3".into()),
    });
    if let Some(provider_id) = provider_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["providerId"] = json!(provider_id);
    }
    if let Some(language) = language
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["language"] = json!(language);
    }
    if let Some(voice) = voice
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["voice"] = json!(voice);
    }
    if let Some(volume) = volume
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["volume"] = json!(volume);
    }
    if let Some(pitch) = pitch
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["pitch"] = json!(pitch);
    }
    if let Some(engine) = engine
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["engine"] = json!(engine);
    }
    if let Some(speed_scale) = speed_scale
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["speedScale"] = json!(speed_scale);
    }
    if let Some(speed) = speed {
        payload["speed"] = json!(speed);
    }
    if let Some(model_dir) = model_dir
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["modelDir"] = json!(model_dir);
    }
    if let Some(python_path) = python_path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["pythonPath"] = json!(python_path);
    }
    if let Some(sample_rate) = sample_rate {
        payload["sampleRate"] = json!(sample_rate);
    }
    if let Some(oral) = oral {
        payload["oral"] = json!(oral);
    }
    if let Some(laugh) = laugh {
        payload["laugh"] = json!(laugh);
    }
    if let Some(break_level) = break_level {
        payload["breakLevel"] = json!(break_level);
    }
    if let Some(speaker_seed) = speaker_seed {
        payload["speakerSeed"] = json!(speaker_seed);
    }
    if let Some(speaker_embedding) = speaker_embedding
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["speakerEmbedding"] = json!(speaker_embedding);
    }
    if let Some(temperature) = temperature {
        payload["temperature"] = json!(temperature);
    }
    if let Some(top_p) = top_p {
        payload["topP"] = json!(top_p);
    }
    if let Some(top_k) = top_k {
        payload["topK"] = json!(top_k);
    }
    if let Some(refine_text_enabled) = refine_text_enabled {
        payload["refineTextEnabled"] = json!(refine_text_enabled);
    }
    if let Some(refine_prompt) = refine_prompt
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        payload["refinePrompt"] = json!(refine_prompt);
    }
    if let Some(refine_temperature) = refine_temperature {
        payload["refineTemperature"] = json!(refine_temperature);
    }
    agent::text_to_speech_payload_for_desktop(&store, &payload).await
}

#[tauri::command(rename_all = "camelCase")]
fn play_chat_audio(path: String) -> AppResult<Value> {
    let path = PathBuf::from(path.trim_matches('"'));
    agent::desktop_voice_playback_start_path(&path)
}

#[tauri::command(rename_all = "camelCase")]
fn stop_chat_audio() -> AppResult<Value> {
    agent::desktop_voice_playback_stop()
}

#[tauri::command(rename_all = "camelCase")]
fn upload_chat_attachment(
    store: State<'_, AppStore>,
    file_name: String,
    mime_type: String,
    bytes: Vec<u8>,
) -> AppResult<Value> {
    save_chat_attachment_bytes(&store, file_name, mime_type, bytes)
}

#[tauri::command(rename_all = "camelCase")]
fn upload_chat_attachment_from_path(store: State<'_, AppStore>, path: String) -> AppResult<Value> {
    let source_path = PathBuf::from(path.trim_matches('"'));
    let metadata = fs::metadata(&source_path).map_err(|error| {
        AppError::BadRequest(format!(
            "attachment source unavailable: {} ({error})",
            source_path.to_string_lossy()
        ))
    })?;
    if !metadata.is_file() {
        return Err(AppError::BadRequest(format!(
            "attachment source is not a file: {}",
            source_path.to_string_lossy()
        )));
    }
    if metadata.len() > MAX_CHAT_ATTACHMENT_BYTES as u64 {
        return Err(AppError::BadRequest(format!(
            "attachment too large: {} bytes",
            metadata.len()
        )));
    }
    let file_name = source_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment")
        .to_string();
    let mime_type = mime_from_attachment_path(&source_path).to_string();
    let bytes = fs::read(&source_path).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read attachment source {}: {error}",
            source_path.to_string_lossy()
        ))
    })?;
    save_chat_attachment_bytes(&store, file_name, mime_type, bytes)
}

fn save_chat_attachment_bytes(
    store: &AppStore,
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

fn mime_from_attachment_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "txt" | "md" | "markdown" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        _ => "application/octet-stream",
    }
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

fn validate_avatar_bytes(bytes: &[u8]) -> AppResult<()> {
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(AppError::BadRequest(
            "avatar image must be between 1 byte and 10 MiB".into(),
        ));
    }
    Ok(())
}

fn normalized_image_ext(file_name: &str) -> AppResult<&'static str> {
    let ext = PathBuf::from(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Ok("png"),
        "jpg" | "jpeg" => Ok("jpg"),
        "webp" => Ok("webp"),
        "gif" => Ok("gif"),
        "bmp" => Ok("bmp"),
        _ => Err(AppError::BadRequest(
            "avatar image must be png, jpg, jpeg, webp, gif, or bmp".into(),
        )),
    }
}

fn image_ext_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

fn remove_file_if_local(path: &str) {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return;
    }
    let path = PathBuf::from(trimmed);
    if path.is_file() {
        let _ = fs::remove_file(path);
    }
}

fn normalize_persona_number(value: &mut Value, key: &str, min: f64, max: f64) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let number = object
        .get(key)
        .and_then(Value::as_f64)
        .unwrap_or(min)
        .clamp(min, max);
    let next = if number.fract() == 0.0 {
        json!(number as u64)
    } else {
        json!(number)
    };
    object.insert(key.to_string(), next);
}

fn normalize_persona_string(value: &mut Value, key: &str, fallback: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let next = object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(fallback)
        .to_string();
    object.insert(key.to_string(), json!(next));
}

fn normalize_persona_bool(value: &mut Value, key: &str, fallback: bool) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let next = object.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    object.insert(key.to_string(), json!(next));
}

fn emoji_root_dir(store: &AppStore) -> AppResult<PathBuf> {
    if let Ok(path) = std::env::var("SYNTHCHAT_EMOJI_DIR") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            let dir = PathBuf::from(trimmed);
            fs::create_dir_all(&dir)?;
            return Ok(dir);
        }
    }
    let dir = store.data_dir().join("emoji");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn bundled_emoji_dir() -> Option<PathBuf> {
    std::env::var("SYNTHCHAT_BUNDLED_EMOJI_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            std::env::current_exe().ok().and_then(|exe| {
                let parent = exe.parent()?;
                [
                    parent.join("data").join("emoji"),
                    parent.join("resources").join("data").join("emoji"),
                    parent.join("resources").join("emoji"),
                ]
                .into_iter()
                .find(|path| path.is_dir())
                .or_else(|| {
                    parent.parent().and_then(|grandparent| {
                        [
                            grandparent.join("data").join("emoji"),
                            grandparent.join("resources").join("data").join("emoji"),
                            grandparent.join("resources").join("emoji"),
                        ]
                        .into_iter()
                        .find(|path| path.is_dir())
                    })
                })
            })
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.join("data").join("emoji"))
                .filter(|path| path.is_dir())
        })
        .or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(|path| path.join("data").join("emoji"))
                .filter(|path| path.is_dir())
        })
}

fn ensure_default_emoji_assets(store: &AppStore) -> AppResult<()> {
    let root = emoji_root_dir(store)?;
    if root
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
    {
        return Ok(());
    }
    if let Some(source) = bundled_emoji_dir() {
        copy_dir_contents(&source, &root)?;
    }
    Ok(())
}

fn copy_dir_contents(source: &std::path::Path, destination: &std::path::Path) -> AppResult<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_contents(&source_path, &destination_path)?;
        } else if source_path.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn scan_emoji_groups(store: &AppStore) -> AppResult<Vec<EmojiGroupConfig>> {
    let root = emoji_root_dir(store)?;
    let mut groups = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let group_dir = entry.path();
        if !group_dir.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let mut emotions = Vec::new();
        let mut images = Vec::new();
        let mut emotion_images = HashMap::new();
        for emotion_entry in fs::read_dir(&group_dir)? {
            let emotion_entry = emotion_entry?;
            let emotion_dir = emotion_entry.path();
            if !emotion_dir.is_dir() {
                continue;
            }
            let emotion = emotion_entry.file_name().to_string_lossy().to_string();
            let mut emotion_files = Vec::new();
            for file in fs::read_dir(&emotion_dir)? {
                let file = file?;
                let path = file.path();
                if path.is_file() && is_supported_emoji_image(&path) {
                    let path = path.to_string_lossy().to_string();
                    images.push(path.clone());
                    emotion_files.push(path);
                }
            }
            emotion_files.sort();
            emotions.push(emotion.clone());
            emotion_images.insert(emotion, emotion_files);
        }
        emotions.sort();
        images.sort();
        groups.push(EmojiGroupConfig {
            id: id.clone(),
            name: id,
            emotions,
            images,
            emotion_images,
        });
    }
    groups.sort_by(|left, right| left.name.cmp(&right.name));
    write_emoji_groups_snapshot(store, &groups)?;
    Ok(groups)
}

fn write_emoji_groups_snapshot(store: &AppStore, groups: &[EmojiGroupConfig]) -> AppResult<()> {
    let path = store.data_dir().join("emoji_groups.json");
    fs::write(path, serde_json::to_vec_pretty(groups)?)?;
    Ok(())
}

fn is_supported_emoji_image(path: &std::path::Path) -> bool {
    if !matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    ) {
        return false;
    }
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn emoji_group_dir(store: &AppStore, group_id: &str) -> AppResult<PathBuf> {
    Ok(emoji_root_dir(store)?.join(validate_emoji_name(group_id)?))
}

fn emoji_emotion_dir(store: &AppStore, group_id: &str, emotion: &str) -> AppResult<PathBuf> {
    Ok(emoji_group_dir(store, group_id)?.join(validate_emoji_name(emotion)?))
}

fn emoji_image_path(
    store: &AppStore,
    group_id: &str,
    emotion: &str,
    file_name: &str,
) -> AppResult<PathBuf> {
    let file_name = file_name
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("emoji file name is required".into()))?;
    normalized_image_ext(file_name)
        .map_err(|_| AppError::BadRequest("unsupported emoji image file".into()))?;
    Ok(emoji_emotion_dir(store, group_id, emotion)?.join(file_name))
}

fn validate_emoji_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 50 {
        return Err(AppError::BadRequest(
            "emoji name must be 1-50 characters".into(),
        ));
    }
    if name.starts_with([' ', '.']) || name.ends_with([' ', '.']) {
        return Err(AppError::BadRequest(
            "emoji name cannot start or end with space/dot".into(),
        ));
    }
    if name
        .chars()
        .any(|ch| matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(AppError::BadRequest(
            "emoji name contains invalid characters".into(),
        ));
    }
    Ok(name.to_string())
}

fn sanitize_emoji_file_stem(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric() || matches!(*ch, '-' | '_' | ' ' | '(' | ')'))
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}

fn unique_emoji_name(store: &AppStore, base: &str) -> AppResult<String> {
    let base = validate_emoji_name(base)?;
    let root = emoji_root_dir(store)?;
    if !root.join(&base).exists() {
        return Ok(base);
    }
    for index in 2..10000 {
        let candidate = format!("{base}_{index}");
        if !root.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Ok(format!("{}_{}", base, new_id("emoji")))
}

fn sync_persona_emoji_group(
    store: &AppStore,
    old_name: &str,
    new_name: Option<&str>,
) -> AppResult<()> {
    let personas = store
        .personas()?
        .into_iter()
        .map(|mut persona| {
            if persona.emoji_group == old_name {
                persona.emoji_group = new_name.unwrap_or("").to_string();
                if new_name.is_none() {
                    persona.emoji_enabled = false;
                }
            }
            persona
        })
        .collect::<Vec<_>>();
    for persona in personas {
        store.save_persona(persona)?;
    }
    Ok(())
}

fn apply_persona_emoji(store: &AppStore, persona: &Persona, reply: String) -> String {
    if !persona.emoji_enabled || persona.emoji_send_probability == 0 || reply.trim().is_empty() {
        return reply;
    }
    let probability = persona.emoji_send_probability.min(100) as u64;
    let roll = (utc_epoch_seconds().wrapping_add(hash_to_u64(&reply))) % 100;
    if roll >= probability {
        return reply;
    }
    let Ok(groups) = scan_emoji_groups(store) else {
        return reply;
    };
    let Some(group) = groups
        .iter()
        .find(|group| group.id == persona.emoji_group || group.name == persona.emoji_group)
    else {
        return reply;
    };
    let available = group
        .emotion_images
        .iter()
        .filter(|(_, images)| !images.is_empty())
        .collect::<Vec<_>>();
    if available.is_empty() {
        return reply;
    }
    let seed = utc_epoch_seconds()
        .wrapping_add(hash_to_u64(&persona.id))
        .wrapping_add(hash_to_u64(&reply));
    let (_, images) = available[(seed as usize) % available.len()];
    let preferred_index = (seed as usize + persona.id.len() + reply.len()) % images.len();
    let path = images
        .iter()
        .cycle()
        .skip(preferred_index)
        .take(images.len())
        .find(|path| Path::new(path.as_str()).is_file());
    let Some(path) = path else {
        return reply;
    };
    let mime = mime_for_image_path(path);
    format!("{reply}\n\n[media attached: \"{path}\" ({mime})]")
}

fn hash_to_u64(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn utc_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn mime_for_image_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "image/png",
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

fn pet_window_target_size(mode: &str) -> LogicalSize<f64> {
    match mode {
        "model" => LogicalSize::new(PET_MODEL_WINDOW_WIDTH, PET_MODEL_WINDOW_HEIGHT),
        "orb" => LogicalSize::new(PET_ORB_WINDOW_WIDTH, PET_ORB_WINDOW_HEIGHT),
        "dock" => LogicalSize::new(PET_DOCK_WINDOW_WIDTH, PET_DOCK_WINDOW_HEIGHT),
        _ => LogicalSize::new(PET_WINDOW_WIDTH, PET_WINDOW_HEIGHT),
    }
}

fn clamp_pet_window_position(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    monitor_origin: &PhysicalPosition<i32>,
    monitor_size: &PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let min_x = monitor_origin.x;
    let max_x = monitor_origin.x + monitor_size.width as i32 - width as i32;
    let min_y = monitor_origin.y + PET_WINDOW_SAFE_MARGIN_TOP;
    let max_y = monitor_origin.y + monitor_size.height as i32
        - height as i32
        - PET_WINDOW_SAFE_MARGIN_BOTTOM;
    PhysicalPosition::new(
        x.clamp(min_x, max_x.max(min_x)),
        y.clamp(min_y, max_y.max(min_y)),
    )
}

fn place_pet_window_for_mode(
    window: &tauri::WebviewWindow,
    mode: &str,
    dock_edge: Option<PetDockEdge>,
) -> AppResult<()> {
    let size = pet_window_target_size(mode);
    let scale_factor = window
        .scale_factor()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let physical_size = size.to_physical::<u32>(scale_factor);
    let current_position = window
        .outer_position()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let current_size = window
        .outer_size()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let current_center_x = current_position.x + current_size.width as i32 / 2;
    let current_bottom_y = current_position.y + current_size.height as i32;
    window
        .set_size(size)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    if let Some(monitor) = window
        .current_monitor()
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    {
        let origin = monitor.position();
        let monitor_size = monitor.size();
        let mut x = current_center_x - physical_size.width as i32 / 2;
        let y = current_bottom_y - physical_size.height as i32;
        if mode == "dock" || mode == "orb" {
            x = match dock_edge.unwrap_or(PetDockEdge::Right) {
                PetDockEdge::Left => origin.x,
                PetDockEdge::Right => {
                    origin.x + monitor_size.width as i32 - physical_size.width as i32
                }
            };
        }
        let next = clamp_pet_window_position(
            x,
            y,
            physical_size.width,
            physical_size.height,
            origin,
            monitor_size,
        );
        let _ = window.set_position(next);
    }
    Ok(())
}

fn clamp_existing_pet_window(window: &tauri::WebviewWindow) -> AppResult<()> {
    let Some(monitor) = window
        .current_monitor()
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    else {
        return Ok(());
    };
    let position = window
        .outer_position()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let size = window
        .outer_size()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let next = clamp_pet_window_position(
        position.x,
        position.y,
        size.width,
        size.height,
        monitor.position(),
        monitor.size(),
    );
    let _ = window.set_position(next);
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn pet_window_set_ignore_cursor_events(app: AppHandle, ignore: bool) -> AppResult<()> {
    let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) else {
        return Ok(());
    };
    window
        .set_ignore_cursor_events(ignore)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(())
}

fn ensure_pet_window(app: &AppHandle, focus: bool) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) {
        window
            .show()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        let _ = clamp_existing_pet_window(&window);
        if focus {
            window
                .set_focus()
                .map_err(|error| AppError::BadRequest(error.to_string()))?;
        }
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        app,
        PET_WINDOW_LABEL,
        WebviewUrl::App("index.html?window=pet".into()),
    )
    .title("SynthPet")
    .inner_size(PET_MODEL_WINDOW_WIDTH, PET_MODEL_WINDOW_HEIGHT)
    .min_inner_size(PET_DOCK_WINDOW_WIDTH, PET_DOCK_WINDOW_HEIGHT)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .shadow(false)
    .focused(false)
    .drag_and_drop(true)
    .build()
    .map_err(|error| AppError::BadRequest(error.to_string()))?;

    if let Some(monitor) = window
        .current_monitor()
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    {
        let origin = monitor.position();
        let size = monitor.size();
        let x = origin.x
            + size
                .width
                .saturating_sub(PET_MODEL_WINDOW_WIDTH as u32 + 24) as i32;
        let y = origin.y
            + size
                .height
                .saturating_sub(PET_MODEL_WINDOW_HEIGHT as u32 + 64) as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }

    window
        .show()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    if focus {
        window
            .set_focus()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
    }

    Ok(())
}

fn setup_tray(app: &App) -> AppResult<()> {
    let open = MenuItemBuilder::with_id(TRAY_OPEN_ID, "打开")
        .build(app)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let pet = MenuItemBuilder::with_id(TRAY_PET_ID, "桌宠")
        .build(app)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let quit = MenuItemBuilder::with_id(TRAY_QUIT_ID, "退出")
        .build(app)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let menu = MenuBuilder::new(app)
        .item(&open)
        .item(&pet)
        .separator()
        .item(&quit)
        .build()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;

    let mut tray_builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("SynthChat")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                let _ = show_main_window(tray.app_handle().clone());
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            TRAY_OPEN_ID => {
                let _ = show_main_window(app.clone());
            }
            TRAY_PET_ID => {
                let _ = ensure_pet_window(app, true);
            }
            TRAY_QUIT_ID => {
                app.exit(0);
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        tray_builder = tray_builder.icon(icon.clone());
    }

    tray_builder
        .build(app)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;

    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
async fn open_pet_window(app: AppHandle) -> AppResult<()> {
    ensure_pet_window(&app, true)
}

fn show_pet_first(app: &AppHandle) -> AppResult<()> {
    ensure_pet_window(app, false)?;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn show_main_window(app: AppHandle) -> AppResult<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    if window
        .is_minimized()
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    {
        window
            .unminimize()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
    }
    window
        .show()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    window
        .set_focus()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn toggle_main_window(app: AppHandle) -> AppResult<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let visible = window
        .is_visible()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let minimized = window
        .is_minimized()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    if visible && !minimized {
        window
            .hide()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        return Ok(());
    }
    if minimized {
        window
            .unminimize()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
    }
    window
        .show()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    window
        .set_focus()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn pet_window_action(app: AppHandle, action: String, edge: Option<String>) -> AppResult<()> {
    let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) else {
        return Ok(());
    };
    match action.as_str() {
        "close" => window
            .close()
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
        "collapse" => {
            place_pet_window_for_mode(&window, "dock", PetDockEdge::from_option(edge.as_deref()))?;
        }
        "expand" => {
            place_pet_window_for_mode(&window, "full", None)?;
        }
        "model" => {
            place_pet_window_for_mode(&window, "model", None)?;
        }
        "dock" => {
            place_pet_window_for_mode(&window, "dock", PetDockEdge::from_option(edge.as_deref()))?;
        }
        "orb" => {
            place_pet_window_for_mode(&window, "orb", PetDockEdge::from_option(edge.as_deref()))?;
        }
        "undock" => {
            place_pet_window_for_mode(&window, "model", None)?;
        }
        "drag" => window
            .start_dragging()
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
        _ => {}
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn cursor_position(app: AppHandle) -> AppResult<Value> {
    let cursor = app
        .cursor_position()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let window = app.get_webview_window(PET_WINDOW_LABEL);
    let (
        window_x,
        window_y,
        window_width,
        window_height,
        screen_x,
        screen_y,
        screen_width,
        screen_height,
    ) = if let Some(window) = window {
        let position = window
            .outer_position()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        let size = window
            .outer_size()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        let monitor = window
            .current_monitor()
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        let (screen_x, screen_y, screen_width, screen_height) = monitor
            .map(|monitor| {
                (
                    monitor.position().x,
                    monitor.position().y,
                    monitor.size().width,
                    monitor.size().height,
                )
            })
            .unwrap_or((0, 0, 0, 0));
        (
            position.x,
            position.y,
            size.width,
            size.height,
            screen_x,
            screen_y,
            screen_width,
            screen_height,
        )
    } else {
        (0, 0, 0, 0, 0, 0, 0, 0)
    };
    Ok(json!({
        "x": cursor.x,
        "y": cursor.y,
        "screenX": cursor.x,
        "screenY": cursor.y,
        "screenWidth": screen_width,
        "screenHeight": screen_height,
        "screenXOrigin": screen_x,
        "screenYOrigin": screen_y,
        "clientX": cursor.x - window_x as f64,
        "clientY": cursor.y - window_y as f64,
        "windowWidth": window_width,
        "windowHeight": window_height,
        "windowScreenX": window_x,
        "windowScreenY": window_y,
    }))
}

fn pet_window_drag_point(
    app: &AppHandle,
    screen_x: Option<f64>,
    screen_y: Option<f64>,
    use_cursor: bool,
    fallback_x: i32,
    fallback_y: i32,
) -> (i32, i32) {
    if use_cursor {
        if let Ok(cursor) = app.cursor_position() {
            return (cursor.x.round() as i32, cursor.y.round() as i32);
        }
    }
    (
        screen_x.unwrap_or(fallback_x as f64).round() as i32,
        screen_y.unwrap_or(fallback_y as f64).round() as i32,
    )
}

#[tauri::command(rename_all = "camelCase")]
fn pet_window_drag(
    app: AppHandle,
    state: State<'_, Mutex<PetDragState>>,
    action: String,
    screen_x: Option<f64>,
    screen_y: Option<f64>,
    use_cursor: Option<bool>,
) -> AppResult<()> {
    let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) else {
        return Ok(());
    };
    let mut drag = state.lock().unwrap();
    let prefer_cursor = use_cursor.unwrap_or(false);
    match action.as_str() {
        "start" => {
            let position = window
                .outer_position()
                .map_err(|error| AppError::BadRequest(error.to_string()))?;
            let (pointer_x, pointer_y) =
                pet_window_drag_point(&app, screen_x, screen_y, prefer_cursor, 0, 0);
            drag.active = true;
            drag.window_x = position.x;
            drag.window_y = position.y;
            drag.pointer_x = pointer_x;
            drag.pointer_y = pointer_y;
        }
        "move" => {
            if !drag.active {
                return Ok(());
            }
            let (x, y) = pet_window_drag_point(
                &app,
                screen_x,
                screen_y,
                prefer_cursor,
                drag.pointer_x,
                drag.pointer_y,
            );
            let next_x = drag.window_x + x - drag.pointer_x;
            let next_y = drag.window_y + y - drag.pointer_y;
            window
                .set_position(PhysicalPosition::new(next_x, next_y))
                .map_err(|error| AppError::BadRequest(error.to_string()))?;
        }
        "end" => {
            *drag = PetDragState::default();
        }
        _ => {}
    }
    Ok(())
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

fn emit_file_drop_event<R: tauri::Runtime>(window: &tauri::Window<R>, event: &DragDropEvent) {
    let label = window.label();
    if label != "main" && label != PET_WINDOW_LABEL {
        return;
    }
    let position_payload = |position: &tauri::PhysicalPosition<f64>| {
        json!({
            "x": position.x,
            "y": position.y,
        })
    };
    let payload = match event {
        DragDropEvent::Enter { paths, position } => json!({
            "type": "enter",
            "paths": paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
            "position": position_payload(position),
            "windowLabel": label,
        }),
        DragDropEvent::Over { position } => json!({
            "type": "over",
            "paths": [],
            "position": position_payload(position),
            "windowLabel": label,
        }),
        DragDropEvent::Drop { paths, position } => json!({
            "type": "drop",
            "paths": paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
            "position": position_payload(position),
            "windowLabel": label,
        }),
        DragDropEvent::Leave => json!({
            "type": "leave",
            "paths": [],
            "windowLabel": label,
        }),
        _ => return,
    };
    let _ = window.emit("synthchat-file-drop-event", payload);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime = synthchat_multi_thread_runtime("synthchat-tauri-worker")
        .expect("failed to initialize SynthChat async runtime");
    tauri::async_runtime::set(runtime.handle().clone());
    let store = AppStore::new(state_path()).expect("failed to initialize SynthChat state");
    sync_runtime_env_from_store(&store);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(store)
        .manage(Mutex::new(PetDragState::default()))
        .manage(Mutex::new(PetVisionState::default()))
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(drop_event) = event {
                emit_file_drop_event(window, drop_event);
            }
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            setup_tray(app)?;
            let store = app.state::<AppStore>();
            mcp::start_mcp_keepalive_loop(store.inner().clone());
            let wechat_store = store.inner().clone();
            let wechat_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wechat_settings::run_wechat_poll_loop(wechat_store, wechat_app).await;
            });
            let proactive_store = store.inner().clone();
            let proactive_app = app.handle().clone();
            std::thread::Builder::new()
                .name("synthchat-proactive-loop".into())
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("failed to build proactive runtime");
                    runtime.block_on(run_proactive_loop(proactive_app, proactive_store));
                })
                .map_err(|error| AppError::Io(error))?;
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
            let app_handle = app.handle().clone();
            if let Err(error) = show_pet_first(&app_handle) {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                eprintln!("failed to show pet window: {error}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_build_info,
            check_app_update,
            install_app_update,
            open_app_update_url,
            get_config,
            save_config,
            add_trusted_tool_pattern,
            remove_trusted_tool_pattern,
            add_hermes_credential_pool_entry,
            list_state_snapshots,
            create_state_snapshot,
            prune_state_snapshots,
            restore_state_snapshot,
            list_workspace_snapshots,
            create_workspace_snapshot,
            restore_workspace_snapshot,
            cleanup_historical_resources,
            capture_screen_base64,
            set_pet_vision_active,
            pick_path,
            get_profile,
            save_profile,
            upload_profile_avatar,
            clear_profile_avatar,
            list_personas,
            get_persona,
            save_persona,
            upload_persona_avatar,
            clear_persona_avatar,
            list_emoji_groups,
            save_emoji_groups,
            upload_emoji_image,
            create_emoji_group,
            rename_emoji_group,
            delete_emoji_group,
            create_emoji_emotion,
            rename_emoji_emotion,
            delete_emoji_emotion,
            delete_emoji_image,
            rename_emoji_image,
            delete_persona,
            list_accounts,
            save_accounts,
            get_wechat_config,
            save_wechat_config,
            start_wechat_qr,
            check_wechat_qr_status,
            list_wechat_links,
            link_wechat_account,
            unlink_wechat_account,
            wechat_poll_once,
            wechat_inbound_text,
            list_conversations,
            create_conversation,
            delete_conversation,
            rename_conversation,
            set_conversation_agent,
            list_messages,
            send_chat_message,
            delete_message,
            list_proactive_statuses,
            trigger_proactive_once,
            list_llm_providers,
            save_llm_providers,
            refresh_model_catalog,
            lookup_model_capabilities,
            infer_provider_model_capabilities,
            get_provider_catalog_info,
            list_agentic_models,
            detect_provider_models,
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
            get_mcp_status,
            reset_mcp_persistent_session,
            remove_mcp_oauth_tokens,
            refresh_mcp_oauth_tokens,
            start_mcp_oauth_login,
            finish_mcp_oauth_login,
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
            list_agent_runtime_events,
            list_managed_processes,
            stop_managed_process,
            browser_runtime_status,
            computer_use_runtime_status,
            list_agent_control_commands,
            list_plugin_auxiliary_tasks,
            list_agent_auxiliary_tasks,
            agent_auxiliary_task_defaults,
            list_agent_auxiliary_task_assignments,
            save_agent_auxiliary_task_assignment,
            reset_agent_auxiliary_task_assignments,
            judge_agent_goal,
            agent_goal_status,
            set_agent_goal,
            pause_agent_goal,
            resume_agent_goal,
            clear_agent_goal,
            add_agent_subgoal,
            remove_agent_subgoal,
            clear_agent_subgoals,
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
            dispatch_kanban_and_drain_agent_queue,
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
            auto_describe_agent,
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
            transcribe_chat_audio,
            speak_chat_text,
            play_chat_audio,
            stop_chat_audio,
            upload_chat_attachment,
            upload_chat_attachment_from_path,
            environment_check,
            empty_list,
            noop,
            passthrough_value,
            asset_url,
            open_pet_window,
            show_main_window,
            toggle_main_window,
            pet_window_action,
            pet_window_drag,
            pet_window_set_ignore_cursor_events,
            cursor_position,
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
    fn mcp_stdio_action_is_detected_from_args() {
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "--mcp-stdio"]),
            Some(AcpCliAction::McpStdio)
        );
        assert_eq!(
            acp_cli_action_from_args(["synthchat", "serve-mcp"]),
            Some(AcpCliAction::McpStdio)
        );
    }

    #[test]
    fn app_update_manifest_accepts_github_release_payload() {
        let manifest = parse_app_update_manifest(json!({
            "tag_name": "v1.1.2",
            "html_url": "https://github.com/Sunner-Chao/SynthChat/releases/tag/v1.1.2",
            "body": "Release notes",
            "published_at": "2026-06-28T00:00:00Z",
            "assets": [
                {"name": "source.zip", "browser_download_url": "https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.2/source.zip"},
                {"name": "SynthChat_1.1.2_x64-setup.exe", "browser_download_url": "https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.2/SynthChat_1.1.2_x64-setup.exe"}
            ]
        }))
        .unwrap();
        assert_eq!(manifest.latest_version, "v1.1.2");
        assert_eq!(
            manifest.download_url.as_deref(),
            Some("https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.2/SynthChat_1.1.2_x64-setup.exe")
        );
        assert_eq!(
            manifest.release_url.as_deref(),
            Some("https://github.com/Sunner-Chao/SynthChat/releases/tag/v1.1.2")
        );
        assert_eq!(manifest.notes.as_deref(), Some("Release notes"));
        assert_eq!(manifest.published_at.as_deref(), Some("2026-06-28T00:00:00Z"));
    }

    #[test]
    fn app_update_manifest_url_normalizes_common_github_urls() {
        assert_eq!(
            normalize_app_update_manifest_url(
                "https://api.github.com/Sunner-Chao/SynthChat/releases/latest/download/update-manifest.json"
            )
            .as_deref(),
            Some("https://github.com/Sunner-Chao/SynthChat/releases/latest/download/update-manifest.json")
        );
        assert_eq!(
            normalize_app_update_manifest_url(
                "https://api.github.com/repos/Sunner-Chao/SynthChat/releases/latest/download/update-manifest.json"
            )
            .as_deref(),
            Some("https://github.com/Sunner-Chao/SynthChat/releases/latest/download/update-manifest.json")
        );
        assert_eq!(
            normalize_app_update_manifest_url(
                "https://api.github.com/Sunner-Chao/SynthChat/releases/latest"
            )
            .as_deref(),
            Some("https://api.github.com/repos/Sunner-Chao/SynthChat/releases/latest")
        );
        assert_eq!(
            normalize_app_update_manifest_url(
                "https://github.com/Sunner-Chao/SynthChat/releases/latest"
            )
            .as_deref(),
            Some("https://github.com/Sunner-Chao/SynthChat/releases/latest/download/update-manifest.json")
        );
    }

    #[test]
    fn app_update_missing_github_manifest_message_names_release_asset() {
        let url = reqwest::Url::parse(
            "https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.0/update-manifest.json",
        )
        .unwrap();
        let message = github_missing_update_manifest_message(&url).unwrap();
        assert!(message.contains("v1.1.0"));
        assert!(message.contains("update-manifest.json"));
    }

    #[test]
    fn app_update_version_compare_ignores_v_prefix_and_numeric_suffixes() {
        assert_eq!(
            compare_app_versions("v1.1.10", "1.1.2"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_app_versions("1.1.0", "v1.1.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_app_versions("1.1.0-beta.1", "1.1.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn app_update_installer_file_name_requires_native_installer_extension() {
        let exe = reqwest::Url::parse(
            "https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.2/SynthChat%201.1.2.exe",
        )
        .unwrap();
        assert_eq!(
            app_update_file_name_from_url(&exe).unwrap(),
            "SynthChat_1.1.2.exe"
        );
        let zip = reqwest::Url::parse(
            "https://github.com/Sunner-Chao/SynthChat/releases/download/v1.1.2/SynthChat.zip",
        )
        .unwrap();
        assert!(app_update_file_name_from_url(&zip).is_err());
    }

    #[test]
    fn mcp_stdio_initialize_ping_and_empty_lists_are_protocol_compatible() {
        let dir = std::env::temp_dir().join(format!("synthchat-mcp-protocol-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let initialize = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": "init",
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-03-26"
                    }
                }),
            ))
            .unwrap();
        assert_eq!(initialize["result"]["protocolVersion"], "2025-03-26");
        assert!(initialize["result"]["capabilities"]["tools"].is_object());
        assert!(initialize["result"]["capabilities"]["resources"].is_object());
        assert!(initialize["result"]["capabilities"]["prompts"].is_object());

        let initialized_notification = runtime.block_on(handle_mcp_stdio_json_rpc(
            &store,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        ));
        assert!(initialized_notification.is_none());

        for (id, method, result_key) in [
            ("ping", "ping", ""),
            ("resources", "resources/list", "resources"),
            ("templates", "resources/templates/list", "resourceTemplates"),
            ("prompts", "prompts/list", "prompts"),
        ] {
            let response = runtime
                .block_on(handle_mcp_stdio_json_rpc(
                    &store,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": method
                    }),
                ))
                .unwrap();
            assert!(response.get("error").is_none());
            if result_key.is_empty() {
                assert!(response["result"].as_object().unwrap().is_empty());
            } else {
                assert!(response["result"][result_key]
                    .as_array()
                    .unwrap()
                    .is_empty());
            }
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_stdio_tools_list_exposes_hermes_style_tool_surface() {
        let dir = std::env::temp_dir().join(format!("synthchat-mcp-stdio-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let response = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/list"
                }),
            ))
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"browser_snapshot"));
        assert!(names.contains(&"vision_analyze"));
        assert!(names.contains(&"text_to_speech"));
        assert!(names.contains(&"kanban_complete"));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["type"] == "object"));
        let web_search = tools
            .iter()
            .find(|tool| tool["name"] == "web_search")
            .expect("web_search should be exposed");
        assert_eq!(
            web_search["annotations"]["source"],
            json!("synthchat-tools")
        );
        assert_eq!(web_search["annotations"]["serverId"], json!("__internal"));
        assert_eq!(
            web_search["inputSchema"]["properties"]["query"]["type"],
            "string"
        );
        assert_eq!(
            web_search["inputSchema"]["properties"]["limit"]["type"],
            "integer"
        );
        let browser_navigate = tools
            .iter()
            .find(|tool| tool["name"] == "browser_navigate")
            .expect("browser_navigate should be exposed");
        assert_eq!(
            browser_navigate["inputSchema"]["properties"]["url"]["type"],
            "string"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_stdio_tools_call_invokes_exposed_internal_tool() {
        let dir = std::env::temp_dir().join(format!("synthchat-mcp-call-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let response = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "voice_status",
                        "arguments": {}
                    }
                }),
            ))
            .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"action\":\"voice_status\""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_stdio_tools_call_accepts_json_string_arguments_and_rejects_unsafe_tools() {
        let dir = std::env::temp_dir().join(format!("synthchat-mcp-call-args-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let string_args = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": "string-args",
                    "method": "tools/call",
                    "params": {
                        "name": "voice_status",
                        "arguments": "{}"
                    }
                }),
            ))
            .unwrap();
        assert_eq!(string_args["result"]["isError"], false);

        let unsafe_tool = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": "unsafe-tool",
                    "method": "tools/call",
                    "params": {
                        "name": "terminal",
                        "arguments": {
                            "command": "echo should-not-run"
                        }
                    }
                }),
            ))
            .unwrap();
        assert_eq!(unsafe_tool["result"]["isError"], true);
        assert!(unsafe_tool["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not exposed"));

        let bad_args = runtime
            .block_on(handle_mcp_stdio_json_rpc(
                &store,
                &json!({
                    "jsonrpc": "2.0",
                    "id": "bad-args",
                    "method": "tools/call",
                    "params": {
                        "name": "voice_status",
                        "arguments": []
                    }
                }),
            ))
            .unwrap();
        assert_eq!(bad_args["result"]["isError"], true);
        assert!(bad_args["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("must be a JSON object"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn github_skill_raw_url_is_derived_from_blob_and_tree_urls() {
        let blob =
            reqwest::Url::parse("https://github.com/owner/repo/blob/main/skills/demo/SKILL.md")
                .unwrap();
        assert_eq!(
            github_blob_skill_raw_url(&blob).as_deref(),
            Some("https://raw.githubusercontent.com/owner/repo/main/skills/demo/SKILL.md")
        );

        let tree =
            reqwest::Url::parse("https://github.com/owner/repo/tree/main/skills/demo").unwrap();
        assert_eq!(
            github_blob_skill_raw_url(&tree).as_deref(),
            Some("https://raw.githubusercontent.com/owner/repo/main/skills/demo/SKILL.md")
        );
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
