use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{AgentDefinition, ImageProvider, LlmProvider, Persona, VideoProvider, VisionProvider},
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{
    list_agent_auxiliary_task_assignments, resolve_workspace_path, truncate_output,
    validate_web_url, workspace_root,
};

static VOICE_PLAYBACK_PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
static VOICE_RECORDING_PROCESS: OnceLock<Mutex<Option<VoiceRecordingProcess>>> = OnceLock::new();

struct VoiceRecordingProcess {
    child: Child,
    path: PathBuf,
    started_at: Instant,
}

pub(super) fn voice_status_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let cleanup = payload
        .get("cleanup")
        .or_else(|| payload.get("cleanupTemp"))
        .or_else(|| payload.get("cleanup_temp"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_age_seconds = payload
        .get("maxAgeSeconds")
        .or_else(|| payload.get("max_age_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(3600)
        .clamp(60, 24 * 3600);
    let audio_capture = local_audio_capture_status();
    let playback = local_audio_playback_status();
    let desktop_stt = desktop_local_stt_status();
    let recording = current_voice_recording_status_value()?;
    let tts = voice_llm_audio_provider_status(store, "text_to_speech")?;
    let stt = voice_llm_audio_provider_status(store, "transcribe_audio")?;
    let cleanup_deleted = if cleanup {
        Some(cleanup_temp_voice_recordings(max_age_seconds)?)
    } else {
        None
    };
    let available = audio_capture["available"].as_bool().unwrap_or(false)
        && stt["available"].as_bool().unwrap_or(false);
    Ok(json!({
        "action": "voice_status",
        "available": available,
        "audioAvailable": audio_capture["available"],
        "sttAvailable": stt["available"],
        "ttsAvailable": tts["available"],
        "audioCapture": audio_capture,
        "playback": playback,
        "desktopLocalStt": desktop_stt,
        "recording": recording,
        "sttProvider": stt,
        "ttsProvider": tts,
        "cleanupDeleted": cleanup_deleted,
        "notes": [
            "Hermes-style voice requirements/status check only; recording actions are not started by this tool.",
            "Use voice_recording for local recording lifecycle, voice_playback for audio playback, transcribe_audio for existing audio files, and text_to_speech for speech generation."
        ]
    })
    .to_string())
}

pub(super) fn voice_playback_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    match action.as_str() {
        "play" | "start" => voice_playback_start(agent, payload),
        "stop" | "cancel" | "interrupt" => voice_playback_stop(),
        "status" | "" => voice_playback_status(),
        other => Err(AppError::BadRequest(format!(
            "unsupported voice_playback action: {other}"
        ))),
    }
}

pub(super) fn voice_recording_tool(payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    match action.as_str() {
        "start" | "record" => voice_recording_start(payload),
        "stop" | "finish" => voice_recording_stop(false),
        "cancel" | "discard" => voice_recording_stop(true),
        "status" | "" => voice_recording_status(),
        other => Err(AppError::BadRequest(format!(
            "unsupported voice_recording action: {other}"
        ))),
    }
}

fn voice_recording_start(payload: &Value) -> AppResult<String> {
    let duration_seconds = payload
        .get("durationSeconds")
        .or_else(|| payload.get("duration_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(3600);
    let path = temp_voice_recording_path()?;
    let mut command = recording_command_for_path(&path, duration_seconds)?;
    command.hide_window();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut guard = voice_recording_process_lock()?;
    stop_voice_recording_locked(&mut guard, true)?;
    let child = command.spawn().map_err(|error| {
        AppError::BadRequest(format!("failed to start voice recording: {error}"))
    })?;
    let process_id = child.id();
    *guard = Some(VoiceRecordingProcess {
        child,
        path: path.clone(),
        started_at: Instant::now(),
    });
    Ok(json!({
        "action": "voice_recording",
        "status": "recording",
        "path": path.to_string_lossy(),
        "processId": process_id,
        "durationSeconds": duration_seconds
    })
    .to_string())
}

fn voice_recording_stop(discard: bool) -> AppResult<String> {
    let mut guard = voice_recording_process_lock()?;
    let stopped = stop_voice_recording_locked(&mut guard, discard)?;
    Ok(json!({
        "action": "voice_recording",
        "status": if discard { "cancelled" } else { "stopped" },
        "stopped": stopped.stopped,
        "path": if discard { None } else { stopped.path },
        "sizeBytes": if discard { None } else { stopped.size_bytes },
        "durationMs": stopped.duration_ms
    })
    .to_string())
}

fn voice_recording_status() -> AppResult<String> {
    Ok(current_voice_recording_status_value()?.to_string())
}

fn current_voice_recording_status_value() -> AppResult<Value> {
    let mut guard = voice_recording_process_lock()?;
    let mut status = "idle";
    let mut path = None;
    let mut process_id = None;
    let mut duration_ms = None;
    if let Some(recording) = guard.as_mut() {
        match recording.child.try_wait() {
            Ok(Some(_)) => {
                let finished_path = recording.path.to_string_lossy().to_string();
                let elapsed = recording.started_at.elapsed().as_millis() as u64;
                *guard = None;
                status = "finished";
                path = Some(finished_path);
                duration_ms = Some(elapsed);
            }
            Ok(None) => {
                status = "recording";
                path = Some(recording.path.to_string_lossy().to_string());
                process_id = Some(recording.child.id());
                duration_ms = Some(recording.started_at.elapsed().as_millis() as u64);
            }
            Err(_) => {
                *guard = None;
            }
        }
    }
    Ok(json!({
        "action": "voice_recording",
        "status": status,
        "path": path,
        "processId": process_id,
        "durationMs": duration_ms
    }))
}

struct StoppedRecording {
    stopped: bool,
    path: Option<String>,
    size_bytes: Option<u64>,
    duration_ms: Option<u64>,
}

fn voice_recording_process_lock(
) -> AppResult<std::sync::MutexGuard<'static, Option<VoiceRecordingProcess>>> {
    VOICE_RECORDING_PROCESS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| AppError::BadRequest("voice recording lock is poisoned".into()))
}

fn stop_voice_recording_locked(
    guard: &mut Option<VoiceRecordingProcess>,
    discard: bool,
) -> AppResult<StoppedRecording> {
    let Some(mut recording) = guard.take() else {
        return Ok(StoppedRecording {
            stopped: false,
            path: None,
            size_bytes: None,
            duration_ms: None,
        });
    };
    if recording.child.try_wait()?.is_none() {
        recording.child.kill()?;
        let _ = recording.child.wait();
    }
    let duration_ms = Some(recording.started_at.elapsed().as_millis() as u64);
    let path = recording.path;
    if discard {
        let _ = fs::remove_file(&path);
        return Ok(StoppedRecording {
            stopped: true,
            path: None,
            size_bytes: None,
            duration_ms,
        });
    }
    let size_bytes = fs::metadata(&path).ok().map(|metadata| metadata.len());
    Ok(StoppedRecording {
        stopped: true,
        path: Some(path.to_string_lossy().to_string()),
        size_bytes,
        duration_ms,
    })
}

fn voice_playback_start(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let source = payload
        .get("path")
        .or_else(|| payload.get("audioPath"))
        .or_else(|| payload.get("audio_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("voice_playback play requires payload.path".into()))?;
    let path = resolve_workspace_path(&workspace_root(agent)?, source)?;
    if !path.is_file() {
        return Err(AppError::BadRequest(format!(
            "voice_playback path is not a file: {}",
            path.display()
        )));
    }
    start_voice_playback_path(&path).map(|value| value.to_string())
}

pub fn desktop_voice_playback_start_path(path: &Path) -> AppResult<Value> {
    if !path.is_file() {
        return Err(AppError::BadRequest(format!(
            "voice playback path is not a file: {}",
            path.display()
        )));
    }
    start_voice_playback_path(path)
}

pub fn desktop_voice_playback_stop() -> AppResult<Value> {
    voice_playback_stop_value()
}

fn start_voice_playback_path(path: &Path) -> AppResult<Value> {
    let mut command = playback_command_for_path(&path)?;
    command.hide_window();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut guard = voice_playback_process_lock()?;
    stop_voice_playback_locked(&mut guard)?;
    let child = command.spawn().map_err(|error| {
        AppError::BadRequest(format!("failed to start voice playback: {error}"))
    })?;
    let process_id = child.id();
    *guard = Some(child);
    Ok(json!({
        "action": "voice_playback",
        "status": "playing",
        "path": path.to_string_lossy(),
        "processId": process_id
    })
    )
}

fn voice_playback_stop() -> AppResult<String> {
    voice_playback_stop_value().map(|value| value.to_string())
}

fn voice_playback_stop_value() -> AppResult<Value> {
    let mut guard = voice_playback_process_lock()?;
    let stopped = stop_voice_playback_locked(&mut guard)?;
    Ok(json!({
        "action": "voice_playback",
        "status": "stopped",
        "stopped": stopped
    }))
}

fn voice_playback_status() -> AppResult<String> {
    let mut guard = voice_playback_process_lock()?;
    let process_id = guard.as_ref().map(Child::id);
    let running = if let Some(child) = guard.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => {
                *guard = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    } else {
        false
    };
    Ok(json!({
        "action": "voice_playback",
        "status": if running { "playing" } else { "idle" },
        "processId": if running { process_id } else { None }
    })
    .to_string())
}

fn voice_playback_process_lock() -> AppResult<std::sync::MutexGuard<'static, Option<Child>>> {
    VOICE_PLAYBACK_PROCESS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| AppError::BadRequest("voice playback lock is poisoned".into()))
}

fn stop_voice_playback_locked(guard: &mut Option<Child>) -> AppResult<bool> {
    let Some(mut child) = guard.take() else {
        return Ok(false);
    };
    if child.try_wait()?.is_none() {
        child.kill()?;
        let _ = child.wait();
    }
    Ok(true)
}

fn playback_command_for_path(path: &Path) -> AppResult<Command> {
    if let Some(configured) = std::env::var("HERMES_LOCAL_AUDIO_PLAYER")
        .ok()
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_AUDIO_PLAYER").ok())
    {
        return configured_playback_command(&configured, path);
    }
    if command_available("ffplay") {
        let mut command = Command::new("ffplay");
        command
            .arg("-nodisp")
            .arg("-autoexit")
            .arg("-loglevel")
            .arg("quiet")
            .arg(path);
        return Ok(command);
    }
    if command_available("mpv") {
        let mut command = Command::new("mpv");
        command.arg("--really-quiet").arg(path);
        return Ok(command);
    }
    if command_available("afplay") {
        let mut command = Command::new("afplay");
        command.arg(path);
        return Ok(command);
    }
    if command_available("aplay") {
        let mut command = Command::new("aplay");
        command.arg("-q").arg(path);
        return Ok(command);
    }
    if cfg!(target_os = "windows") && command_available("powershell") {
        let mut command = Command::new("powershell");
        let escaped = path.to_string_lossy().replace('\'', "''");
        command.arg("-NoProfile").arg("-Command").arg(format!(
            "$player = New-Object System.Media.SoundPlayer '{}'; $player.PlaySync()",
            escaped
        ));
        return Ok(command);
    }
    Err(AppError::BadRequest(
        "no audio player available; install ffplay/mpv/afplay/aplay or set HERMES_LOCAL_AUDIO_PLAYER"
            .into(),
    ))
}

fn configured_playback_command(configured: &str, path: &Path) -> AppResult<Command> {
    let configured = configured.trim();
    if configured.is_empty() {
        return Err(AppError::BadRequest(
            "HERMES_LOCAL_AUDIO_PLAYER is empty".into(),
        ));
    }
    let mut parts = configured.split_whitespace();
    let Some(program) = parts.next() else {
        return Err(AppError::BadRequest(
            "HERMES_LOCAL_AUDIO_PLAYER is empty".into(),
        ));
    };
    let mut command = Command::new(program);
    let mut inserted_path = false;
    for part in parts {
        if part.contains("{path}") {
            command.arg(part.replace("{path}", &path.to_string_lossy()));
            inserted_path = true;
        } else {
            command.arg(part);
        }
    }
    if !inserted_path {
        command.arg(path);
    }
    Ok(command)
}

fn recording_command_for_path(path: &Path, duration_seconds: u64) -> AppResult<Command> {
    if let Some(configured) = std::env::var("HERMES_LOCAL_MIC_COMMAND")
        .ok()
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_MIC_COMMAND").ok())
    {
        return configured_recording_command(&configured, path, duration_seconds);
    }
    if command_available("termux-microphone-record") {
        let mut command = Command::new("termux-microphone-record");
        command.arg("-f").arg(path);
        if duration_seconds > 0 {
            command.arg("-l").arg(duration_seconds.to_string());
        }
        return Ok(command);
    }
    if command_available("arecord") {
        let mut command = Command::new("arecord");
        command.arg("-f").arg("cd").arg("-t").arg("wav");
        if duration_seconds > 0 {
            command.arg("-d").arg(duration_seconds.to_string());
        }
        command.arg(path);
        return Ok(command);
    }
    if command_available("rec") {
        let mut command = Command::new("rec");
        command.arg(path);
        if duration_seconds > 0 {
            command
                .arg("trim")
                .arg("0")
                .arg(duration_seconds.to_string());
        }
        return Ok(command);
    }
    Err(AppError::BadRequest(
        "no audio recorder available; set HERMES_LOCAL_MIC_COMMAND/SYNTHCHAT_LOCAL_MIC_COMMAND or install termux-microphone-record/arecord/rec"
            .into(),
    ))
}

fn configured_recording_command(
    configured: &str,
    path: &Path,
    duration_seconds: u64,
) -> AppResult<Command> {
    configured_command_with_path(
        configured,
        path,
        "HERMES_LOCAL_MIC_COMMAND",
        Some(("duration", duration_seconds.to_string())),
    )
}

fn configured_command_with_path(
    configured: &str,
    path: &Path,
    label: &str,
    extra_placeholder: Option<(&str, String)>,
) -> AppResult<Command> {
    let configured = configured.trim();
    if configured.is_empty() {
        return Err(AppError::BadRequest(format!("{label} is empty")));
    }
    let mut parts = configured.split_whitespace();
    let Some(program) = parts.next() else {
        return Err(AppError::BadRequest(format!("{label} is empty")));
    };
    let mut command = Command::new(program);
    let mut inserted_path = false;
    for part in parts {
        let mut arg = part.replace("{path}", &path.to_string_lossy());
        if let Some((key, value)) = extra_placeholder.as_ref() {
            arg = arg.replace(&format!("{{{key}}}"), value);
        }
        if part.contains("{path}") {
            inserted_path = true;
        }
        command.arg(arg);
    }
    if !inserted_path {
        command.arg(path);
    }
    Ok(command)
}

fn temp_voice_recording_path() -> AppResult<PathBuf> {
    let timestamp = timestamp_millis()?;
    Ok(std::env::temp_dir().join(format!("recording_{timestamp}.wav")))
}

fn timestamp_millis() -> AppResult<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AppError::BadRequest(format!("invalid system clock: {error}")))?
        .as_millis())
}

fn local_audio_capture_status() -> Value {
    let command = if cfg!(target_os = "windows") {
        std::env::var("HERMES_LOCAL_MIC_COMMAND")
            .ok()
            .or_else(|| std::env::var("SYNTHCHAT_LOCAL_MIC_COMMAND").ok())
    } else {
        std::env::var("HERMES_LOCAL_MIC_COMMAND")
            .ok()
            .or_else(|| std::env::var("SYNTHCHAT_LOCAL_MIC_COMMAND").ok())
            .or_else(|| command_available("arecord").then_some("arecord".into()))
            .or_else(|| command_available("rec").then_some("rec".into()))
    };
    let termux = command_available("termux-microphone-record");
    let available = command.is_some() || termux;
    json!({
        "available": available,
        "backend": if termux { "termux" } else if command.is_some() { "command" } else { "none" },
        "commandConfigured": command.is_some(),
        "termuxMicrophoneRecord": termux,
        "requirements": if available {
            Value::Null
        } else {
            json!("Set HERMES_LOCAL_MIC_COMMAND/SYNTHCHAT_LOCAL_MIC_COMMAND or install a platform audio recorder.")
        }
    })
}

fn local_audio_playback_status() -> Value {
    let command = std::env::var("HERMES_LOCAL_AUDIO_PLAYER")
        .ok()
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_AUDIO_PLAYER").ok())
        .or_else(|| {
            ["ffplay", "mpv", "afplay", "aplay", "powershell"]
                .iter()
                .find(|candidate| command_available(candidate))
                .map(|candidate| (*candidate).to_string())
        });
    json!({
        "available": command.is_some(),
        "command": command.unwrap_or_else(|| "none".into())
    })
}

pub(super) fn desktop_local_stt_status() -> Value {
    let command = desktop_local_stt_command_template();
    json!({
        "available": command.is_some(),
        "backend": if std::env::var("HERMES_LOCAL_STT_COMMAND").ok().or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_COMMAND").ok()).is_some() {
            "command"
        } else if python_module_available("faster_whisper") {
            "faster_whisper"
        } else if command_available("whisper") {
            "whisper_cli"
        } else {
            "none"
        },
        "commandConfigured": std::env::var("HERMES_LOCAL_STT_COMMAND").ok().or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_COMMAND").ok()).is_some(),
        "requirements": if command.is_some() {
            Value::Null
        } else {
            json!("Configure SYNTHCHAT_LOCAL_STT_COMMAND, install faster-whisper, or install a whisper CLI.")
        }
    })
}

fn python_module_available(module: &str) -> bool {
    let script = format!("import importlib.util, sys; sys.exit(0 if importlib.util.find_spec({}) else 1)", python_string_literal(module));
    Command::new("python")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn voice_llm_audio_provider_status(store: &AppStore, capability: &str) -> AppResult<Value> {
    let providers = store.providers()?;
    let provider = providers
        .iter()
        .filter(|provider| voice_provider_supports_capability(provider, capability))
        .max_by_key(|provider| voice_provider_status_rank(provider, capability))
        .cloned()
        .or_else(|| store.provider(None).ok());
    let Some(provider) = provider else {
        return Ok(json!({
            "available": false,
            "configured": false,
            "capability": capability,
            "reason": "no default LLM provider configured"
        }));
    };
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    let supports_capability = voice_provider_supports_capability(&provider, capability);
    let needs_base_url = voice_provider_needs_base_url(&provider_type);
    let base_url_configured = !provider.base_url.trim().is_empty() || !needs_base_url;
    let needs_credential = voice_provider_needs_credential(&provider_type);
    let credential_configured =
        !needs_credential || provider_api_key(&provider.api_key, &provider.api_key_env).is_some();
    let configured = supports_capability && base_url_configured && credential_configured;
    let available = configured && provider.enabled;
    Ok(json!({
        "available": available,
        "configured": configured,
        "capability": capability,
        "providerId": provider.id,
        "providerType": provider.provider_type,
        "model": provider.model,
        "enabled": provider.enabled,
        "baseUrlConfigured": base_url_configured,
        "credentialConfigured": credential_configured,
        "explicitProviderRequired": configured && !provider.enabled,
        "reason": if available {
            Value::Null
        } else if configured && !provider.enabled {
            json!("provider is configured but disabled; pass providerId explicitly or enable it for default voice use")
        } else if !supports_capability {
            json!("provider type does not support this voice capability")
        } else if !base_url_configured {
            json!("provider base URL is not configured")
        } else {
            json!("provider API key is not configured")
        }
    }))
}

fn voice_provider_status_rank(provider: &LlmProvider, capability: &str) -> u8 {
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    let base_url_configured =
        !voice_provider_needs_base_url(&provider_type) || !provider.base_url.trim().is_empty();
    let credential_configured = !voice_provider_needs_credential(&provider_type)
        || provider_api_key(&provider.api_key, &provider.api_key_env).is_some();
    let readiness = match (provider.enabled, base_url_configured, credential_configured) {
        (true, true, true) => 4,
        (false, true, true) => 3,
        (true, true, false) | (true, false, true) => 2,
        _ => 0,
    };
    if voice_provider_is_dedicated_audio_provider(provider, capability) {
        readiness + 4
    } else {
        readiness
    }
}

fn voice_provider_is_dedicated_audio_provider(provider: &LlmProvider, capability: &str) -> bool {
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    let provider_id = provider.id.trim().to_ascii_lowercase();
    match capability {
        "text_to_speech" => {
            provider_id.contains("tts")
                || matches!(
                    provider_type.as_str(),
                    "xai"
                        | "x-ai"
                        | "grok"
                        | "mistral"
                        | "voxtral"
                        | "gemini"
                        | "google-gemini"
                        | "google_gemini"
                        | "minimax"
                        | "minimax-tts"
                        | "minimax_tts"
                        | "elevenlabs"
                        | "eleven_labs"
                        | "edge"
                        | "edge_tts"
                        | "edge-tts"
                        | "piper"
                        | "kittentts"
                        | "kitten_tts"
                        | "kitten-tts"
                        | "neutts"
                        | "neu_tts"
                        | "neu-tts"
                        | "local_command"
                        | "command_tts"
                        | "tts-command"
                        | "command"
                )
        }
        "transcribe_audio" => {
            provider_id.contains("stt")
                || provider_id.contains("transcribe")
                || matches!(
                    provider_type.as_str(),
                    "xai"
                        | "x-ai"
                        | "grok"
                        | "elevenlabs"
                        | "eleven_labs"
                        | "scribe"
                        | "mistral"
                        | "voxtral"
                        | "local_command"
                        | "command_stt"
                        | "stt-command"
                        | "command"
                )
        }
        _ => false,
    }
}

fn voice_provider_supports_capability(provider: &LlmProvider, capability: &str) -> bool {
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    match capability {
        "text_to_speech" => matches!(
            provider_type.as_str(),
            "openai"
                | "openai-compatible"
                | "openai_compatible"
                | "compatible"
                | "custom"
                | ""
                | "xai"
                | "x-ai"
                | "grok"
                | "mistral"
                | "voxtral"
                | "gemini"
                | "google-gemini"
                | "google_gemini"
                | "minimax"
                | "minimax-tts"
                | "minimax_tts"
                | "elevenlabs"
                | "eleven_labs"
                | "edge"
                | "edge_tts"
                | "edge-tts"
                | "piper"
                | "kittentts"
                | "kitten_tts"
                | "kitten-tts"
                | "neutts"
                | "neu_tts"
                | "neu-tts"
                | "local_command"
                | "command_tts"
                | "tts-command"
                | "command"
        ),
        "transcribe_audio" => matches!(
            provider_type.as_str(),
            "openai"
                | "openai-compatible"
                | "openai_compatible"
                | "compatible"
                | "custom"
                | ""
                | "local_command"
                | "command_stt"
                | "stt-command"
                | "command"
                | "xai"
                | "x-ai"
                | "grok"
                | "elevenlabs"
                | "eleven_labs"
                | "scribe"
                | "mistral"
                | "voxtral"
        ),
        _ => false,
    }
}

fn voice_provider_needs_base_url(provider_type: &str) -> bool {
    !matches!(
        provider_type,
        "mistral"
            | "voxtral"
            | "gemini"
            | "google-gemini"
            | "google_gemini"
            | "minimax"
            | "minimax-tts"
            | "minimax_tts"
            | "elevenlabs"
            | "eleven_labs"
            | "scribe"
            | "edge"
            | "edge_tts"
            | "edge-tts"
            | "piper"
            | "kittentts"
            | "kitten_tts"
            | "kitten-tts"
            | "neutts"
            | "neu_tts"
            | "neu-tts"
            | "local_command"
            | "command_tts"
            | "tts-command"
            | "command_stt"
            | "stt-command"
            | "command"
    )
}

fn voice_provider_needs_credential(provider_type: &str) -> bool {
    !matches!(
        provider_type,
        "local_command"
            | "command_tts"
            | "tts-command"
            | "command_stt"
            | "stt-command"
            | "command"
            | "edge"
            | "edge_tts"
            | "edge-tts"
            | "piper"
            | "kittentts"
            | "kitten_tts"
            | "kitten-tts"
            | "neutts"
            | "neu_tts"
            | "neu-tts"
    )
}

fn command_available(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var).any(|dir| {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        if cfg!(target_os = "windows") {
            ["exe", "cmd", "bat", "ps1"]
                .iter()
                .any(|ext| dir.join(format!("{command}.{ext}")).is_file())
        } else {
            false
        }
    })
}

fn cleanup_temp_voice_recordings(max_age_seconds: u64) -> AppResult<usize> {
    let temp_dir = std::env::temp_dir();
    let now = std::time::SystemTime::now();
    let mut deleted = 0usize;
    for entry in fs::read_dir(temp_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with("recording_") && name.ends_with(".wav")) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default().as_secs();
        if age > max_age_seconds {
            fs::remove_file(path)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

pub(super) async fn image_generate_tool(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("image_generate requires payload.prompt".into()))?;
    let persona = persona_for_image_generation(store, conversation_id)?;
    if !persona_image_generation_enabled(&persona) {
        return Err(AppError::BadRequest(format!(
            "image generation is disabled for persona {}",
            persona.name
        )));
    }
    let provider = image_provider_for_persona(store, &persona)?;
    let model = image_model_for_persona(&provider, &persona, payload)?;
    let effective_prompt = compose_persona_image_prompt(&persona, prompt);
    let effective_payload = payload_with_image_model(payload, &model);

    match image_provider_kind(&provider) {
        ImageProviderKind::OpenAiCompatible => {
            openai_compatible_image_generate(
                store,
                run_id,
                &provider,
                &effective_prompt,
                &effective_payload,
            )
            .await
        }
        ImageProviderKind::Gemini => {
            gemini_image_generate(store, run_id, &provider, &model, &effective_prompt).await
        }
        ImageProviderKind::NovelAi => Err(AppError::BadRequest(
            "NovelAI image provider is configured, but the direct NovelAI image adapter is not implemented yet. Use an OpenAI-compatible image endpoint or proxy for now."
                .into(),
        )),
        ImageProviderKind::Unsupported(kind) => Err(AppError::BadRequest(format!(
            "unsupported image provider type: {kind}"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImageProviderKind {
    OpenAiCompatible,
    Gemini,
    NovelAi,
    Unsupported(String),
}

fn persona_for_image_generation(store: &AppStore, conversation_id: &str) -> AppResult<Persona> {
    let conversation = store.conversation(conversation_id)?;
    store.persona(conversation.persona_id.as_deref())
}

fn persona_image_generation_enabled(persona: &Persona) -> bool {
    image_generation_bool(persona, "enabled", false)
}

fn image_provider_for_persona(store: &AppStore, persona: &Persona) -> AppResult<ImageProvider> {
    let configured_provider_id =
        image_generation_string(persona, "provider").unwrap_or_default();
    let providers = store.image_providers()?;
    let provider = if configured_provider_id.is_empty() {
        providers
            .into_iter()
            .find(|provider| provider.enabled && !provider.base_url.trim().is_empty())
            .ok_or_else(|| AppError::BadRequest("no enabled image provider configured".into()))?
    } else {
        providers
            .into_iter()
            .find(|provider| provider.id == configured_provider_id)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "persona image provider not found: {configured_provider_id}"
                ))
            })?
    };

    if !provider.enabled {
        return Err(AppError::BadRequest(format!(
            "persona image provider is disabled: {}",
            provider.name
        )));
    }
    if provider.base_url.trim().is_empty() {
        return Err(AppError::BadRequest(format!(
            "persona image provider URL is empty: {}",
            provider.name
        )));
    }
    Ok(provider)
}

fn image_model_for_persona(
    provider: &ImageProvider,
    persona: &Persona,
    payload: &Value,
) -> AppResult<String> {
    let provider_model = provider.model.trim();
    if !provider_model.is_empty() {
        return Ok(provider_model.to_string());
    }
    payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let model = image_generation_string(persona, "model")?;
            if model.is_empty() {
                None
            } else {
                Some(model)
            }
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "image model is empty for provider {}",
                provider.name
            ))
        })
}

fn payload_with_image_model(payload: &Value, model: &str) -> Value {
    let mut next = payload.clone();
    if let Some(object) = next.as_object_mut() {
        object.insert("model".into(), Value::String(model.to_string()));
    }
    next
}

fn image_generation_string(persona: &Persona, key: &str) -> Option<String> {
    persona
        .image_generation
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn image_generation_bool(persona: &Persona, key: &str, default: bool) -> bool {
    persona
        .image_generation
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn compose_persona_image_prompt(persona: &Persona, prompt: &str) -> String {
    let mut parts = Vec::new();
    if let Some(prefix) = image_generation_string(persona, "stylePrefix") {
        parts.push(prefix);
    }
    parts.push(prompt.trim().to_string());
    if matches!(
        image_generation_string(persona, "refMode").as_deref(),
        Some("avatar")
    ) {
        parts.push("Keep the current persona's established visual identity and avatar style when possible.".into());
    }
    if let Some(art_style) = image_generation_string(persona, "artStyle") {
        parts.push(format!("Style: {art_style}"));
    }
    if image_generation_bool(persona, "negativeEnabled", true) {
        if let Some(negative_prompt) = image_generation_string(persona, "negativePrompt") {
            parts.push(format!("Avoid: {negative_prompt}"));
        }
    }
    parts.join("\n")
}

fn image_provider_kind(provider: &ImageProvider) -> ImageProviderKind {
    match provider
        .provider_type
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "" | "openai" | "openai_image" | "openai_compatible" | "compatible" | "custom"
        | "dalle" | "dall_e" => ImageProviderKind::OpenAiCompatible,
        "gemini" | "gemini_image" | "google_gemini" => ImageProviderKind::Gemini,
        "novelai" | "novel_ai" => ImageProviderKind::NovelAi,
        other => ImageProviderKind::Unsupported(other.to_string()),
    }
}

fn image_http_client(provider: &ImageProvider) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0");
    if !provider.use_system_proxy {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build image client: {error}")))
}

pub(super) async fn openai_compatible_image_generate(
    store: &AppStore,
    run_id: &str,
    provider: &ImageProvider,
    prompt: &str,
    payload: &Value,
) -> AppResult<String> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid image provider URL: {error}")))?;
    if !url.path().ends_with("/images/generations") {
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push_str("/images/generations");
        url.set_path(&path);
    }
    let size = payload
        .get("size")
        .and_then(Value::as_str)
        .unwrap_or("1024x1024");
    let count = payload
        .get("n")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 4);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&provider.model);
    let client = image_http_client(provider)?;
    if model.trim().eq_ignore_ascii_case("gpt-image-2") {
        return gpt_image_2_generate(store, run_id, provider, &client, url, prompt, payload).await;
    }
    let mut body = json!({
        "model": model,
        "prompt": prompt,
        "size": size,
        "n": count,
        "response_format": "b64_json"
    });
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }
    let mut request = client.post(url.clone()).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("image_generate failed: {error}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read image response: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "image_generate returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid image JSON: {error}")))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::BadRequest("image response missing data array".into()))?;
    let mut artifacts = Vec::new();
    for item in data {
        if let Some(b64) = item.get("b64_json").and_then(Value::as_str) {
            let bytes = decode_base64_image(b64)?;
            let path = store.save_tool_binary_artifact(run_id, "image_generate", "png", &bytes)?;
            artifacts.push(json!({"path": path.to_string_lossy(), "source": "b64_json", "sizeBytes": bytes.len()}));
        } else if let Some(image_url) = item.get("url").and_then(Value::as_str) {
            validate_web_url(image_url)?;
            let (bytes, extension) = download_image_bytes(&client, image_url).await?;
            let path =
                store.save_tool_binary_artifact(run_id, "image_generate", &extension, &bytes)?;
            artifacts.push(json!({"path": path.to_string_lossy(), "source": image_url, "sizeBytes": bytes.len()}));
        }
    }
    if artifacts.is_empty() {
        return Err(AppError::BadRequest(
            "image response did not contain b64_json or url".into(),
        ));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "prompt": prompt,
        "artifacts": artifacts
    }))?)
}

async fn gpt_image_2_generate(
    store: &AppStore,
    run_id: &str,
    provider: &ImageProvider,
    client: &reqwest::Client,
    submit_url: reqwest::Url,
    prompt: &str,
    payload: &Value,
) -> AppResult<String> {
    let body = gpt_image_2_request_body(prompt, payload);
    let mut request = client.post(submit_url).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("gpt-image-2 submit failed: {error}")))?;
    let submit = response_json_or_error(response, "gpt-image-2 submit").await?;
    let task_id = gpt_image_2_task_id(&submit).ok_or_else(|| {
        AppError::BadRequest(format!(
            "gpt-image-2 submit response missing task_id: {}",
            truncate_output(&submit.to_string(), 2000)
        ))
    })?;
    let result = poll_gpt_image_2_task(client, provider, &task_id).await?;
    let image_urls = gpt_image_2_result_urls(&result);
    if image_urls.is_empty() {
        return Err(AppError::BadRequest(format!(
            "gpt-image-2 completed without result image URLs: {}",
            truncate_output(&result.to_string(), 2000)
        )));
    }
    let mut artifacts = Vec::new();
    for image_url in image_urls {
        validate_web_url(&image_url)?;
        let (bytes, extension) = download_image_bytes(client, &image_url).await?;
        let path = store.save_tool_binary_artifact(run_id, "image_generate", &extension, &bytes)?;
        artifacts.push(json!({
            "path": path.to_string_lossy(),
            "source": image_url,
            "sizeBytes": bytes.len()
        }));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": "gpt-image-2",
        "prompt": prompt,
        "taskId": task_id,
        "task": result,
        "artifacts": artifacts
    }))?)
}

fn gpt_image_2_request_body(prompt: &str, payload: &Value) -> Value {
    let mut body = json!({
        "model": "gpt-image-2",
        "prompt": prompt,
        "n": 1,
        "size": gpt_image_2_size(payload),
        "resolution": gpt_image_2_resolution(payload)
    });
    for key in [
        "background",
        "moderation",
        "output_format",
        "outputFormat",
        "partial_images",
        "partialImages",
        "stream",
        "image",
        "images",
        "image_urls",
        "imageUrls",
        "input_image",
        "inputImage",
        "reference_images",
        "referenceImages",
    ] {
        if let Some(value) = payload.get(key) {
            if let Some(object) = body.as_object_mut() {
                object.insert(key.to_string(), value.clone());
            }
        }
    }
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(object) = body.as_object_mut() {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
    }
    body
}

fn gpt_image_2_size(payload: &Value) -> String {
    let size = payload
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("1:1");
    match size {
        "1024x1024" | "1024*1024" | "1x1" => "1:1".into(),
        "1024x1792" | "9x16" => "9:16".into(),
        "1792x1024" | "16x9" => "16:9".into(),
        value => value.to_string(),
    }
}

fn gpt_image_2_resolution(payload: &Value) -> String {
    payload
        .get("resolution")
        .or_else(|| payload.get("quality"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "1k" | "2k" | "4k"))
        .unwrap_or("1k")
        .to_string()
}

fn gpt_image_2_task_id(value: &Value) -> Option<String> {
    if let Some(task_id) = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| {
            item.get("task_id")
                .or_else(|| item.get("taskId"))
                .or_else(|| item.get("id"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(task_id.to_string());
    }
    ["task_id", "taskId", "id", "data.task_id", "data.taskId", "data.id"]
        .into_iter()
        .find_map(|path| json_path_string(value, path))
}

async fn poll_gpt_image_2_task(
    client: &reqwest::Client,
    provider: &ImageProvider,
    task_id: &str,
) -> AppResult<Value> {
    let interval = 3u64;
    let max_wait = provider.timeout_seconds.max(300);
    let started = Instant::now();
    loop {
        let url = gpt_image_2_task_url(provider, task_id)?;
        let mut request = client.get(url);
        if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::BadRequest(format!("gpt-image-2 status failed: {error}")))?;
        let value = response_json_or_error(response, "gpt-image-2 status").await?;
        let status = json_path_string(&value, "data.status")
            .or_else(|| json_path_string(&value, "status"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if status == "completed" || !gpt_image_2_result_urls(&value).is_empty() {
            return Ok(value);
        }
        if matches!(
            status.as_str(),
            "failed" | "error" | "canceled" | "cancelled"
        ) {
            let error = json_path_string(&value, "data.error.message")
                .or_else(|| json_path_string(&value, "error.message"))
                .unwrap_or_else(|| truncate_output(&value.to_string(), 2000));
            return Err(AppError::BadRequest(format!(
                "gpt-image-2 failed with status '{status}': {error}"
            )));
        }
        if started.elapsed().as_secs() >= max_wait {
            return Err(AppError::BadRequest(format!(
                "gpt-image-2 timed out after {max_wait}s waiting for task {task_id}"
            )));
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

fn gpt_image_2_task_url(provider: &ImageProvider, task_id: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid image provider URL: {error}")))?;
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/images/generations") {
        path.truncate(path.len() - "/images/generations".len());
    }
    path.push_str("/tasks/");
    path.push_str(task_id.trim());
    url.set_path(&path);
    Ok(url)
}

fn gpt_image_2_result_urls(value: &Value) -> Vec<String> {
    let Some(images) = value
        .pointer("/data/result/images")
        .or_else(|| value.pointer("/result/images"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    for image in images {
        if let Some(url) = image.get("url").and_then(Value::as_str) {
            urls.push(url.to_string());
        } else if let Some(items) = image.get("url").and_then(Value::as_array) {
            for item in items {
                if let Some(url) = item.as_str() {
                    urls.push(url.to_string());
                }
            }
        }
    }
    urls
}

async fn gemini_image_generate(
    store: &AppStore,
    run_id: &str,
    provider: &ImageProvider,
    model: &str,
    prompt: &str,
) -> AppResult<String> {
    let api_key = provider_api_key(&provider.api_key, &provider.api_key_env).ok_or_else(|| {
        AppError::BadRequest(format!(
            "Gemini image provider requires an API key: {}",
            provider.name
        ))
    })?;
    let mut base = provider.base_url.trim().trim_end_matches('/').to_string();
    if base.ends_with("/models") {
        base.truncate(base.len() - "/models".len());
    }
    let model_path = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };
    let mut url = reqwest::Url::parse(&format!("{base}/{model_path}:generateContent"))
        .map_err(|error| AppError::BadRequest(format!("invalid Gemini image URL: {error}")))?;
    url.query_pairs_mut().append_pair("key", &api_key);

    let body = json!({
        "contents": [
            {
                "role": "user",
                "parts": [
                    {"text": prompt}
                ]
            }
        ],
        "generationConfig": {
            "responseModalities": ["TEXT", "IMAGE"]
        }
    });
    let client = image_http_client(provider)?;
    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("gemini image_generate failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Gemini image response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "gemini image_generate returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid Gemini image JSON: {error}")))?;
    let candidates = value
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::BadRequest("Gemini image response missing candidates".into()))?;
    let mut artifacts = Vec::new();
    let mut text_parts = Vec::new();
    for candidate in candidates {
        let parts = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    text_parts.push(text.trim().to_string());
                }
            }
            let inline_data = part
                .get("inlineData")
                .or_else(|| part.get("inline_data"));
            if let Some(inline_data) = inline_data {
                let b64 = inline_data
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AppError::BadRequest("Gemini inline image missing data".into())
                    })?;
                let mime_type = inline_data
                    .get("mimeType")
                    .or_else(|| inline_data.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("image/png");
                let bytes = decode_base64_image(b64)?;
                let extension = image_extension_from_content_type(mime_type);
                let path =
                    store.save_tool_binary_artifact(run_id, "image_generate", &extension, &bytes)?;
                artifacts.push(json!({
                    "path": path.to_string_lossy(),
                    "source": "gemini_inline_data",
                    "mimeType": mime_type,
                    "sizeBytes": bytes.len()
                }));
            }
        }
    }
    if artifacts.is_empty() {
        return Err(AppError::BadRequest(
            "Gemini image response did not contain inline image data".into(),
        ));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "prompt": prompt,
        "text": text_parts,
        "artifacts": artifacts
    }))?)
}

const MAX_VIDEO_GENERATE_DOWNLOAD_BYTES: usize = 200 * 1024 * 1024;

pub(super) async fn video_generate_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let prompt = required_string_arg(payload, &["prompt"], "video_generate")?;
    let provider = store
        .enabled_video_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled video provider configured".into()))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build video client: {error}")))?;
    let body = video_generate_request_body(&provider, &prompt, payload);
    let submit_url = video_provider_submit_url(&provider)?;
    let submit = send_video_provider_request(&client, &provider, submit_url.clone(), &body).await?;
    let final_response = if !provider.status_path.trim().is_empty() {
        poll_video_provider_result(&client, &provider, &submit).await?
    } else {
        submit
    };
    let result_url = video_provider_result_url(&provider, &final_response);
    let mut artifact = None;
    if provider.download_result {
        if let Some(url) = result_url.as_deref() {
            let (bytes, extension, mime) = download_generated_video_bytes(&client, url).await?;
            let path =
                store.save_tool_binary_artifact(run_id, "video_generate", &extension, &bytes)?;
            artifact = Some(json!({
                "path": path.to_string_lossy(),
                "source": url,
                "mimeType": mime,
                "sizeBytes": bytes.len(),
            }));
        }
    }
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": video_provider_model(&provider, payload),
        "prompt": prompt,
        "submitUrl": submit_url.to_string(),
        "videoUrl": result_url,
        "artifact": artifact,
        "raw": final_response,
    }))?)
}

pub(super) fn video_generate_request_body(
    provider: &VideoProvider,
    prompt: &str,
    payload: &Value,
) -> Value {
    let mut body = json!({
        "model": video_provider_model(provider, payload),
        "prompt": prompt,
        "operation": string_arg(payload, &["operation"]).unwrap_or_else(|| "generate".into()),
    });
    let mapped = [
        ("image_url", ["imageUrl", "image_url"].as_slice()),
        ("video_url", ["videoUrl", "video_url"].as_slice()),
        (
            "negative_prompt",
            ["negativePrompt", "negative_prompt"].as_slice(),
        ),
        ("aspect_ratio", ["aspectRatio", "aspect_ratio"].as_slice()),
        ("resolution", ["resolution"].as_slice()),
    ];
    if let Some(obj) = body.as_object_mut() {
        for (target, keys) in mapped {
            if let Some(value) = string_arg(payload, keys) {
                obj.insert(target.into(), Value::String(value));
            }
        }
        for key in ["duration", "audio", "seed"] {
            if let Some(value) = payload.get(key) {
                obj.insert(key.into(), value.clone());
            }
        }
        if let Some(value) = payload
            .get("referenceImageUrls")
            .or_else(|| payload.get("reference_image_urls"))
        {
            obj.insert("reference_image_urls".into(), value.clone());
        }
        if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
            for (key, value) in extra {
                obj.insert(key.clone(), value.clone());
            }
        }
    }
    body
}

pub(super) fn video_provider_model(provider: &VideoProvider, payload: &Value) -> String {
    string_arg(payload, &["model"]).unwrap_or_else(|| provider.model.clone())
}

pub(super) fn video_provider_submit_url(provider: &VideoProvider) -> AppResult<reqwest::Url> {
    let path = if provider.submit_path.trim().is_empty() {
        match provider.provider_type.trim().to_lowercase().as_str() {
            "openai" | "openai-compatible" | "compatible" | "" => "/videos/generations",
            _ => "",
        }
    } else {
        provider.submit_path.trim()
    };
    video_provider_url(provider, path, None)
}

pub(super) fn video_provider_status_url(
    provider: &VideoProvider,
    task_id: &str,
) -> AppResult<reqwest::Url> {
    let path = provider.status_path.trim().replace("{id}", task_id);
    video_provider_url(provider, &path, Some(task_id))
}

pub(super) fn video_provider_url(
    provider: &VideoProvider,
    path_or_url: &str,
    task_id: Option<&str>,
) -> AppResult<reqwest::Url> {
    if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        return reqwest::Url::parse(path_or_url)
            .map_err(|error| AppError::BadRequest(format!("invalid video provider URL: {error}")));
    }
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid video provider URL: {error}")))?;
    let mut path = url.path().trim_end_matches('/').to_string();
    if !path_or_url.trim().is_empty() {
        path.push('/');
        path.push_str(path_or_url.trim().trim_start_matches('/'));
    }
    if let Some(task_id) = task_id {
        if !path.contains(task_id) && !task_id.trim().is_empty() {
            path.push('/');
            path.push_str(task_id);
        }
    }
    url.set_path(&path);
    Ok(url)
}

pub(super) async fn send_video_provider_request(
    client: &reqwest::Client,
    provider: &VideoProvider,
    url: reqwest::Url,
    body: &Value,
) -> AppResult<Value> {
    let mut request = client.post(url.clone()).json(body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("video_generate failed: {error}")))?;
    response_json_or_error(response, "video_generate").await
}

pub(super) async fn fetch_video_provider_status(
    client: &reqwest::Client,
    provider: &VideoProvider,
    url: reqwest::Url,
) -> AppResult<Value> {
    let mut request = client.get(url.clone());
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("video_generate status failed: {error}")))?;
    response_json_or_error(response, "video_generate status").await
}

pub(super) async fn response_json_or_error(
    response: reqwest::Response,
    label: &str,
) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    if text.trim().is_empty() {
        return Ok(json!(null));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))
}

pub(super) async fn poll_video_provider_result(
    client: &reqwest::Client,
    provider: &VideoProvider,
    submit: &Value,
) -> AppResult<Value> {
    let task_id = video_provider_task_id(provider, submit).ok_or_else(|| {
        AppError::BadRequest("video_generate response missing task id for polling".into())
    })?;
    let status_field = if provider.status_field.trim().is_empty() {
        "status"
    } else {
        provider.status_field.trim()
    };
    let completed = normalized_statuses(
        &provider.completed_statuses,
        &["completed", "succeeded", "success", "ready", "done"],
    );
    let failed = normalized_statuses(
        &provider.failed_statuses,
        &["failed", "error", "canceled", "cancelled"],
    );
    let interval = provider.poll_interval_seconds.max(1).min(30);
    let max_wait = provider
        .max_poll_seconds
        .max(provider.timeout_seconds)
        .max(interval);
    let started = Instant::now();
    loop {
        let status_url = video_provider_status_url(provider, &task_id)?;
        let value = fetch_video_provider_status(client, provider, status_url).await?;
        let status = json_path_string(&value, status_field)
            .or_else(|| {
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
            .to_lowercase();
        if completed.contains(status.trim())
            || video_provider_result_url(provider, &value).is_some()
        {
            return Ok(value);
        }
        if failed.contains(status.trim()) {
            return Err(AppError::BadRequest(format!(
                "video_generate failed with status '{status}': {}",
                truncate_output(&value.to_string(), 2000)
            )));
        }
        if started.elapsed().as_secs() >= max_wait {
            return Err(AppError::BadRequest(format!(
                "video_generate timed out after {max_wait}s waiting for task {task_id}"
            )));
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

pub(super) fn video_provider_task_id(provider: &VideoProvider, value: &Value) -> Option<String> {
    let candidates = if provider.id_path.trim().is_empty() {
        vec!["id", "task_id", "request_id", "prediction.id"]
    } else {
        vec![provider.id_path.trim()]
    };
    candidates
        .into_iter()
        .find_map(|path| json_path_string(value, path))
}

pub(super) fn video_provider_result_url(provider: &VideoProvider, value: &Value) -> Option<String> {
    let candidates = if provider.result_path.trim().is_empty() {
        vec![
            "video.url",
            "video_url",
            "url",
            "output.url",
            "output.video_url",
            "output.0",
            "data.video.url",
            "data.video_url",
        ]
    } else {
        vec![provider.result_path.trim()]
    };
    candidates.into_iter().find_map(|path| {
        let url = json_path_string(value, path)?;
        validate_web_url(&url).ok()?;
        Some(url)
    })
}

pub(super) fn normalized_statuses(values: &[String], defaults: &[&str]) -> HashSet<String> {
    let source = if values.is_empty() {
        defaults.iter().map(|value| (*value).to_string()).collect()
    } else {
        values.to_vec()
    };
    source
        .into_iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

pub(super) fn json_path_string(value: &Value, path: &str) -> Option<String> {
    let mut current = value;
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        if let Ok(index) = segment.parse::<usize>() {
            current = current.as_array()?.get(index)?;
        } else {
            current = current.get(segment)?;
        }
    }
    match current {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub(super) async fn download_generated_video_bytes(
    client: &reqwest::Client,
    source: &str,
) -> AppResult<(Vec<u8>, String, String)> {
    validate_web_url(source)?;
    let response = client.get(source).send().await.map_err(|error| {
        AppError::BadRequest(format!("failed to download generated video: {error}"))
    })?;
    if !response.status().is_success() {
        return Err(AppError::BadRequest(format!(
            "generated video download returned HTTP {}",
            response.status().as_u16()
        )));
    }
    if let Some(length) = response.content_length() {
        if length as usize > MAX_VIDEO_GENERATE_DOWNLOAD_BYTES {
            return Err(AppError::BadRequest(format!(
                "generated video is too large: {} bytes exceeds {} bytes",
                length, MAX_VIDEO_GENERATE_DOWNLOAD_BYTES
            )));
        }
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read generated video: {error}"))
    })?;
    if bytes.len() > MAX_VIDEO_GENERATE_DOWNLOAD_BYTES {
        return Err(AppError::BadRequest(format!(
            "generated video is too large: {} bytes exceeds {} bytes",
            bytes.len(),
            MAX_VIDEO_GENERATE_DOWNLOAD_BYTES
        )));
    }
    let mime =
        video_mime_from_source(source, Some(&content_type)).unwrap_or_else(|| "video/mp4".into());
    let extension = video_extension_from_mime(&mime).unwrap_or_else(|| "mp4".into());
    Ok((bytes.to_vec(), extension, mime))
}

pub(super) fn video_extension_from_mime(mime: &str) -> Option<String> {
    match mime.split(';').next().unwrap_or(mime).trim() {
        "video/mp4" => Some("mp4".into()),
        "video/webm" => Some("webm".into()),
        "video/quicktime" => Some("mov".into()),
        "video/x-matroska" => Some("mkv".into()),
        "video/x-msvideo" => Some("avi".into()),
        _ => None,
    }
}

pub(super) async fn text_to_speech_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let text = string_arg(payload, &["text", "input", "content"])
        .ok_or_else(|| AppError::BadRequest("text_to_speech requires payload.text".into()))?;
    if text.chars().count() > 4096 {
        return Err(AppError::BadRequest(
            "text_to_speech text exceeds 4096 characters".into(),
        ));
    }
    let provider = match payload
        .get("providerId")
        .or_else(|| payload.get("provider_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(provider_id) => store.provider(Some(provider_id))?,
        None => store.provider(None)?,
    };
    if provider.provider_type == "echo" {
        return Err(AppError::BadRequest(
            "text_to_speech requires an enabled OpenAI-compatible provider".into(),
        ));
    }
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "custom" | "" => {
            if provider.base_url.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "text_to_speech requires an enabled OpenAI-compatible provider".into(),
                ));
            }
            openai_compatible_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "xai" | "x-ai" | "grok" => {
            xai_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "mistral" | "voxtral" => {
            mistral_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "gemini" | "google-gemini" | "google_gemini" => {
            gemini_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "minimax" | "minimax-tts" | "minimax_tts" => {
            minimax_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "elevenlabs" | "eleven_labs" => {
            elevenlabs_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        "edge" | "edge_tts" | "edge-tts" => {
            edge_text_to_speech(store, run_id, &provider, &text, payload)
        }
        "piper" | "kittentts" | "kitten_tts" | "kitten-tts" | "neutts" | "neu_tts" | "neu-tts" => {
            local_python_engine_text_to_speech(store, run_id, &provider, &text, payload)
        }
        "local_command" | "command_tts" | "tts-command" | "command" => {
            local_command_text_to_speech(store, run_id, &provider, &text, payload)
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported text_to_speech provider type: {other}"
        ))),
    }
}

pub(super) fn desktop_text_to_speech(
    store: &AppStore,
    run_id: &str,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let mut errors = Vec::new();
    let engine = desktop_tts_engine(payload);
    let explicit_engine = desktop_tts_engine_explicit(payload);
    if engine == "edge" || engine == "edge_tts" || engine == "edge-tts" {
        let provider = desktop_voice_provider("desktop-edge-tts", "edge", "", "", 90);
        let edge_payload = desktop_edge_tts_payload(payload);
        return edge_text_to_speech(store, run_id, &provider, text, &edge_payload);
    }
    if let Some(command) = std::env::var("SYNTHCHAT_LOCAL_TTS_COMMAND")
        .ok()
        .or_else(|| std::env::var("HERMES_LOCAL_TTS_COMMAND").ok())
        .filter(|value| !value.trim().is_empty())
        .filter(|_| !explicit_engine || desktop_tts_engine_is_local_command(&engine))
    {
        let provider = desktop_voice_provider("desktop-local-tts", "local_command", "", &command, 180);
        match local_command_text_to_speech(store, run_id, &provider, text, payload) {
            Ok(value) => return Ok(value),
            Err(error) => errors.push(format!("local command: {error}")),
        }
    }
    if let Some(command) = desktop_chattts_command_template(payload) {
        let provider = desktop_voice_provider("desktop-chattts", "local_command", "", &command, 240);
        match local_command_text_to_speech(store, run_id, &provider, text, payload) {
            Ok(value) => return Ok(value),
            Err(error) => errors.push(format!("ChatTTS: {error}")),
        }
    }
    let provider = desktop_voice_provider("desktop-edge-tts", "edge", "", "", 90);
    let edge_payload = desktop_edge_tts_payload(payload);
    match edge_text_to_speech(store, run_id, &provider, text, &edge_payload) {
        Ok(value) => Ok(value),
        Err(error) => {
            errors.push(format!("Edge TTS: {error}"));
            Err(AppError::BadRequest(format!(
                "No desktop TTS backend succeeded: {}",
                errors.join("; ")
            )))
        }
    }
}

fn desktop_tts_engine_explicit(payload: &Value) -> bool {
    payload
        .get("engine")
        .or_else(|| payload.get("provider"))
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn desktop_tts_engine_is_local_command(engine: &str) -> bool {
    matches!(
        engine,
        "local_command"
            | "command"
            | "command_tts"
            | "tts-command"
            | "fish"
            | "fish_tts"
            | "fish-tts"
            | "indextts"
            | "index_tts"
            | "index-tts"
    )
}

fn desktop_tts_engine(payload: &Value) -> String {
    payload
        .get("engine")
        .or_else(|| payload.get("provider"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("chattts")
        .to_lowercase()
}

fn env_path_value(keys: &[&str]) -> Option<PathBuf> {
    keys.iter()
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn push_path_with_ancestors(paths: &mut Vec<PathBuf>, root: PathBuf) {
    paths.push(root.clone());
    for ancestor in root.ancestors().skip(1).take(6) {
        paths.push(ancestor.to_path_buf());
    }
}

fn dedupe_existing_path(path: PathBuf, seen: &mut HashSet<String>) -> Option<PathBuf> {
    let normalized = path
        .canonicalize()
        .unwrap_or_else(|_| path.clone())
        .to_string_lossy()
        .to_ascii_lowercase();
    seen.insert(normalized).then_some(path)
}

fn push_payload_path_candidates(candidates: &mut Vec<PathBuf>, value: &str, roots: &[PathBuf]) {
    let path = PathBuf::from(value);
    candidates.push(path.clone());
    if path.is_relative() {
        for root in roots {
            candidates.push(root.join(&path));
        }
    }
}

fn resolve_desktop_chattts_script() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_path_value(&[
        "SYNTHCHAT_CHATTTS_SCRIPT",
        "SYNTHCHAT_TTS_SCRIPT",
        "HERMES_CHATTTS_SCRIPT",
        "HERMES_TTS_SCRIPT",
    ]) {
        candidates.push(path);
    }
    let mut roots = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        push_path_with_ancestors(&mut roots, current_dir);
    }
    if let Some(exe_dir) = current_exe_dir() {
        push_path_with_ancestors(&mut roots, exe_dir);
    }
    for root in roots {
        candidates.push(root.join("data").join("tts").join("chattts_synth.py"));
        candidates.push(
            root.join("resources")
                .join("data")
                .join("tts")
                .join("chattts_synth.py"),
        );
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|path| dedupe_existing_path(path, &mut seen))
        .find(|path| path.is_file())
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn resolve_desktop_chattts_model_dir(payload: &Value) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let mut roots = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        push_path_with_ancestors(&mut roots, current_dir);
    }
    if let Some(exe_dir) = current_exe_dir() {
        push_path_with_ancestors(&mut roots, exe_dir);
    }
    if let Some(model_dir) = payload_string(payload, &["modelDir", "model_dir"]) {
        push_payload_path_candidates(&mut candidates, &model_dir, &roots);
    }
    if let Some(path) = env_path_value(&[
        "SYNTHCHAT_CHATTTS_MODEL_DIR",
        "SYNTHCHAT_TTS_MODEL_DIR",
        "HERMES_CHATTTS_MODEL_DIR",
        "HERMES_TTS_MODEL_DIR",
        "CHAT_TTS_MODEL_DIR",
        "CHATTTS_MODEL_DIR",
    ]) {
        candidates.push(path);
    }
    for root in roots {
        candidates.push(root.join("models").join("ChatTTS"));
        candidates.push(root.join("ChatTTS"));
        candidates.push(root.join("resources").join("models").join("ChatTTS"));
        candidates.push(root.join("resources").join("ChatTTS"));
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|path| dedupe_existing_path(path, &mut seen))
        .find(|path| path.exists())
}

fn resolve_desktop_chattts_speaker_embedding(value: &str, model_dir: &Path) -> Option<String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return path
            .is_file()
            .then(|| path.to_string_lossy().to_string());
    }

    let mut roots = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        push_path_with_ancestors(&mut roots, current_dir);
    }
    if let Some(exe_dir) = current_exe_dir() {
        push_path_with_ancestors(&mut roots, exe_dir);
    }
    roots.push(model_dir.to_path_buf());
    if let Some(parent) = model_dir.parent() {
        roots.push(parent.to_path_buf());
    }

    let mut seen = HashSet::new();
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join(&path),
                root.join("speaker").join(&path),
                root.join("ChatTTS").join(&path),
                root.join("models").join("ChatTTS").join(&path),
            ]
        })
        .filter_map(|candidate| dedupe_existing_path(candidate, &mut seen))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().to_string())
}

fn desktop_edge_tts_payload(payload: &Value) -> Value {
    let mut next = payload.clone();
    let uses_chattts_scale = payload
        .get("speedScale")
        .or_else(|| payload.get("speed_scale"))
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case("chattts"))
        .unwrap_or(false);
    if uses_chattts_scale {
        if let Some(speed) = payload.get("speed").and_then(Value::as_f64) {
            let edge_speed = (1.0 + (speed.clamp(1.0, 9.0) - 5.0) * 0.125).clamp(0.5, 1.5);
            next["speed"] = json!(edge_speed);
        }
    }
    next
}

fn desktop_voice_provider(
    id: &str,
    provider_type: &str,
    model: &str,
    base_url: &str,
    timeout_seconds: u64,
) -> LlmProvider {
    let mut provider = LlmProvider::default();
    provider.id = id.into();
    provider.name = id.into();
    provider.provider_type = provider_type.into();
    provider.model = model.into();
    provider.base_url = base_url.into();
    provider.timeout_seconds = timeout_seconds;
    provider.enabled = true;
    provider
}

fn desktop_chattts_command_template(payload: &Value) -> Option<String> {
    let engine = desktop_tts_engine(payload);
    if !matches!(engine.as_str(), "chattts" | "chat_tts") {
        return None;
    }
    let script = resolve_desktop_chattts_script()?;
    let model_path = resolve_desktop_chattts_model_dir(payload)?;
    let model_dir = model_path.to_string_lossy().to_string();
    let python = payload
        .get("pythonPath")
        .or_else(|| payload.get("python_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("SYNTHCHAT_CHATTTS_PYTHON")
                .ok()
                .or_else(|| std::env::var("SYNTHCHAT_TTS_PYTHON").ok())
                .or_else(|| std::env::var("HERMES_CHATTTS_PYTHON").ok())
                .or_else(|| std::env::var("HERMES_TTS_PYTHON").ok())
        })
        .unwrap_or_else(default_desktop_python_command);
    let sample_rate = payload
        .get("sampleRate")
        .or_else(|| payload.get("sample_rate"))
        .and_then(Value::as_u64)
        .unwrap_or(24000);
    let speed = payload
        .get("speed")
        .and_then(Value::as_f64)
        .map(|value| value.round() as i64)
        .unwrap_or(5)
        .clamp(1, 9);
    let oral = payload
        .get("oral")
        .and_then(Value::as_i64)
        .unwrap_or(2)
        .clamp(0, 9);
    let laugh = payload
        .get("laugh")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 9);
    let break_level = payload
        .get("breakLevel")
        .or_else(|| payload.get("break_level"))
        .and_then(Value::as_i64)
        .unwrap_or(4)
        .clamp(0, 9);
    let speaker_seed = payload
        .get("speakerSeed")
        .or_else(|| payload.get("speaker_seed"))
        .and_then(Value::as_u64)
        .unwrap_or(20240);
    let speaker_embedding = payload
        .get("speakerEmbedding")
        .or_else(|| payload.get("speaker_embedding"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            let path = Path::new(value);
            let looks_like_embedding = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "pt" | "pth" | "safetensors"
                    )
                });
            if looks_like_embedding {
                resolve_desktop_chattts_speaker_embedding(value, &model_path)
            } else {
                Some(value.to_string())
            }
        });
    let temperature = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .unwrap_or(0.3)
        .clamp(0.01, 2.0);
    let top_p = payload
        .get("topP")
        .or_else(|| payload.get("top_p"))
        .and_then(Value::as_f64)
        .unwrap_or(0.7)
        .clamp(0.01, 1.0);
    let top_k = payload
        .get("topK")
        .or_else(|| payload.get("top_k"))
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100);
    let refine_text_enabled = payload
        .get("refineTextEnabled")
        .or_else(|| payload.get("refine_text_enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let refine_prompt = payload
        .get("refinePrompt")
        .or_else(|| payload.get("refine_prompt"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let refine_temperature = payload
        .get("refineTemperature")
        .or_else(|| payload.get("refine_temperature"))
        .and_then(Value::as_f64)
        .unwrap_or(0.7)
        .clamp(0.01, 2.0);
    let speaker_seed_arg = if speaker_seed > 0 {
        format!(" --speaker-seed {}", speaker_seed)
    } else {
        String::new()
    };
    let speaker_embedding_arg = speaker_embedding
        .map(|value| format!(" --speaker-embedding {}", shell_quote_value(&value)))
        .unwrap_or_default();
    let refine_text_arg = if refine_text_enabled {
        "--refine-text"
    } else {
        "--no-refine-text"
    };
    let refine_prompt_arg = refine_prompt
        .map(|value| format!(" --refine-prompt {}", shell_quote_value(&value)))
        .unwrap_or_default();
    let output_format = tts_response_format(payload).unwrap_or_else(|_| "wav".into());
    let silk_arg = if output_format == "silk" {
        "--silk"
    } else {
        "--no-silk"
    };
    Some(format!(
        "{} {} --text-file {{input_path}} --out {{output_path}} --sample-rate {} --model-dir {} --speed {} --oral {} --laugh {} --break-level {}{}{} --temperature {} --top-p {} --top-k {} {}{} --refine-temperature {} {}",
        shell_quote_value(&python),
        shell_quote_path(&script),
        sample_rate,
        shell_quote_value(&model_dir),
        speed,
        oral,
        laugh,
        break_level,
        speaker_seed_arg,
        speaker_embedding_arg,
        temperature,
        top_p,
        top_k,
        refine_text_arg,
        refine_prompt_arg,
        refine_temperature,
        silk_arg
    ))
}

pub(super) async fn openai_compatible_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = audio_speech_url(provider)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "gpt-4o-mini-tts"
            } else {
                configured
            }
        });
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("alloy");
    let format = tts_response_format(payload)?;
    let mut body = json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": format,
    });
    if let Some(speed) = payload.get("speed").and_then(Value::as_f64) {
        if !(0.25..=4.0).contains(&speed) {
            return Err(AppError::BadRequest(
                "text_to_speech speed must be between 0.25 and 4.0".into(),
            ));
        }
        body["speed"] = json!(speed);
    }
    if let Some(instructions) = payload
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body["instructions"] = json!(instructions);
    }
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build TTS client: {error}")))?;
    let mut request = client.post(url.clone()).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("text_to_speech failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read TTS response: {error}")))?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let audio = if content_type.contains("json") {
        decode_tts_json_response(&bytes)?
    } else {
        bytes.to_vec()
    };
    if audio.is_empty() {
        return Err(AppError::BadRequest(
            "text_to_speech returned empty audio".into(),
        ));
    }
    let path = store.save_tool_binary_artifact(run_id, "text_to_speech", &format, &audio)?;
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "voice": voice,
        "format": format,
        "artifact": {
            "path": path.to_string_lossy(),
            "sizeBytes": audio.len()
        }
    }))?)
}

pub(super) async fn xai_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let api_key = provider_api_key(&provider.api_key, &provider.api_key_env).ok_or_else(|| {
        AppError::BadRequest("xAI TTS requires xAI OAuth credentials or XAI_API_KEY".into())
    })?;
    let format = tts_response_format(payload)?;
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("grok-voice");
    let language = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("en");
    let mut body = json!({
        "text": text,
        "voice_id": voice,
        "language": language,
    });
    if format != "mp3" {
        body["output_format"] = json!({"codec": format});
    }
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }
    let url = xai_tts_url(provider)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build xAI TTS client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI text_to_speech failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read xAI TTS response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "xAI text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let audio = if content_type.contains("json") {
        decode_tts_json_response(&bytes)?
    } else {
        bytes.to_vec()
    };
    if audio.is_empty() {
        return Err(AppError::BadRequest(
            "xAI text_to_speech returned empty audio".into(),
        ));
    }
    let path = store.save_tool_binary_artifact(run_id, "text_to_speech", &format, &audio)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "xai",
        "providerId": provider.id,
        "model": provider.model,
        "voice": voice,
        "language": language,
        "format": format,
        "artifact": {
            "path": path.to_string_lossy(),
            "sizeBytes": audio.len()
        }
    }))?)
}

pub(super) fn xai_tts_url(provider: &LlmProvider) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid xAI TTS provider URL: {error}")))?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/tts") {
        return Ok(url);
    }
    let path = if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else if path.ends_with("/audio/speech") {
        path.trim_end_matches("/audio/speech").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/tts");
    url.set_path(&next);
    Ok(url)
}

pub(super) async fn mistral_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_mistral_audio_credential(store, provider)?;
    let format = tts_response_format(payload)?;
    let response_format = if format == "ogg" { "opus" } else { &format };
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "voxtral-mini-tts-2603"
            } else {
                configured
            }
        });
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("c69964a6-ab8b-4f8a-9465-ec0925096ec8");
    let mut body = json!({
        "model": model,
        "input": text,
        "voice_id": voice,
        "response_format": response_format,
    });
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }
    let url = mistral_audio_speech_url(&credential.base_url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Mistral TTS client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .bearer_auth(&credential.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Mistral text_to_speech failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Mistral TTS response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "Mistral text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let audio = if content_type.contains("json") || bytes.first() == Some(&b'{') {
        decode_mistral_tts_json_response(&bytes)?
    } else {
        bytes.to_vec()
    };
    if audio.is_empty() {
        return Err(AppError::BadRequest(
            "Mistral text_to_speech returned empty audio".into(),
        ));
    }
    let path = store.save_tool_binary_artifact(run_id, "text_to_speech", &format, &audio)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "mistral",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "voice": voice,
        "format": format,
        "responseFormat": response_format,
        "artifact": {
            "path": path.to_string_lossy(),
            "sizeBytes": audio.len()
        }
    }))?)
}

pub(super) fn mistral_audio_speech_url(base_url: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid Mistral TTS provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/audio/speech") {
        return Ok(url);
    }
    let path = if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else if path.ends_with("/audio/transcriptions") {
        path.trim_end_matches("/audio/transcriptions").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/audio/speech");
    url.set_path(&next);
    Ok(url)
}

pub(super) fn decode_mistral_tts_json_response(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use base64::Engine;
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| AppError::BadRequest(format!("invalid Mistral TTS JSON: {error}")))?;
    let encoded = value
        .get("audio_data")
        .or_else(|| value.get("audio"))
        .or_else(|| value.get("b64_json"))
        .or_else(|| value.get("data"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::BadRequest("Mistral TTS JSON response missing audio data".into())
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| AppError::BadRequest(format!("invalid Mistral TTS audio base64: {error}")))
}

pub(super) async fn gemini_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_gemini_audio_credential(store, provider)?;
    let requested_format = tts_response_format(payload)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "gemini-2.5-flash-preview-tts"
            } else {
                configured
            }
        });
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceName"))
        .or_else(|| payload.get("voice_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Kore");
    let body = json!({
        "contents": [{
            "parts": [{
                "text": text
            }]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice
                    }
                }
            }
        }
    });
    let mut url = gemini_generate_content_url(&credential.base_url, model)?;
    url.query_pairs_mut()
        .append_pair("key", &credential.api_key);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Gemini TTS client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Gemini text_to_speech failed: {error}")))?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Gemini TTS response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "Gemini text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let pcm = decode_gemini_tts_pcm_response(&bytes)?;
    let audio = wrap_pcm_as_wav(&pcm, 24_000, 1, 2)?;
    let artifact = finalize_tts_audio(store, run_id, "gemini", &audio, "wav", &requested_format)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "gemini",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "voice": voice,
        "format": requested_format,
        "actualFormat": artifact.format,
        "voiceCompatible": artifact.voice_compatible,
        "voice_compatible": artifact.voice_compatible,
        "mediaTag": tts_media_tag(&artifact),
        "media_tag": tts_media_tag(&artifact),
        "mimeType": tts_audio_mime(&artifact.format),
        "conversion": artifact.conversion,
        "artifact": {
            "path": artifact.path.to_string_lossy(),
            "sizeBytes": artifact.size
        }
    }))?)
}

#[derive(Clone, Debug)]
struct GeminiAudioCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_gemini_audio_credential(
    store: &AppStore,
    provider: &LlmProvider,
) -> AppResult<GeminiAudioCredential> {
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        return Ok(GeminiAudioCredential {
            api_key,
            base_url: gemini_audio_base_url(provider, None),
            source: if provider.api_key.is_some() {
                format!("provider:{}", provider.id)
            } else {
                format!("env:{}", provider.api_key_env)
            },
        });
    }
    if let Some((api_key, source)) = std::env::var("GEMINI_API_KEY")
        .ok()
        .map(|value| (value.trim().to_string(), "env:GEMINI_API_KEY".to_string()))
        .or_else(|| {
            std::env::var("GOOGLE_API_KEY")
                .ok()
                .map(|value| (value.trim().to_string(), "env:GOOGLE_API_KEY".to_string()))
        })
        .filter(|(value, _)| !value.is_empty())
    {
        return Ok(GeminiAudioCredential {
            api_key,
            base_url: gemini_audio_base_url(provider, None),
            source,
        });
    }
    let config = store.config()?;
    if let Some((api_key, source)) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| {
            env.get("GEMINI_API_KEY")
                .and_then(Value::as_str)
                .map(|value| {
                    (
                        value.trim().to_string(),
                        "dashboardEnv:GEMINI_API_KEY".to_string(),
                    )
                })
                .or_else(|| {
                    env.get("GOOGLE_API_KEY")
                        .and_then(Value::as_str)
                        .map(|value| {
                            (
                                value.trim().to_string(),
                                "dashboardEnv:GOOGLE_API_KEY".to_string(),
                            )
                        })
                })
        })
        .filter(|(value, _)| !value.is_empty())
    {
        let dashboard_base = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| env.get("GEMINI_BASE_URL").and_then(Value::as_str));
        return Ok(GeminiAudioCredential {
            api_key,
            base_url: gemini_audio_base_url(provider, dashboard_base),
            source,
        });
    }
    Err(AppError::BadRequest(
        "GEMINI_API_KEY or GOOGLE_API_KEY is not set for Gemini TTS".into(),
    ))
}

fn gemini_audio_base_url(provider: &LlmProvider, dashboard_base_url: Option<&str>) -> String {
    let provider_base = provider.base_url.trim();
    if !provider_base.is_empty() {
        return provider_base.trim_end_matches('/').to_string();
    }
    std::env::var("GEMINI_BASE_URL")
        .ok()
        .or_else(|| dashboard_base_url.map(str::to_string))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".into())
}

pub(super) fn gemini_generate_content_url(base_url: &str, model: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid Gemini TTS provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with(":generateContent") {
        return Ok(url);
    }
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/models/");
    next.push_str(model.trim_matches('/'));
    next.push_str(":generateContent");
    url.set_path(&next);
    Ok(url)
}

pub(super) fn decode_gemini_tts_pcm_response(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use base64::Engine;
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| AppError::BadRequest(format!("invalid Gemini TTS JSON: {error}")))?;
    let parts = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::BadRequest("Gemini TTS response missing content parts".into()))?;
    let encoded = parts
        .iter()
        .find_map(|part| {
            part.get("inlineData")
                .or_else(|| part.get("inline_data"))
                .and_then(|inline| inline.get("data"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("Gemini TTS response contained no audio data".into())
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| AppError::BadRequest(format!("invalid Gemini TTS audio base64: {error}")))
}

pub(super) fn wrap_pcm_as_wav(
    pcm: &[u8],
    sample_rate: u32,
    channels: u16,
    sample_width: u16,
) -> AppResult<Vec<u8>> {
    let byte_rate = sample_rate
        .checked_mul(channels as u32)
        .and_then(|value| value.checked_mul(sample_width as u32))
        .ok_or_else(|| AppError::BadRequest("invalid WAV byte rate".into()))?;
    let block_align = channels
        .checked_mul(sample_width)
        .ok_or_else(|| AppError::BadRequest("invalid WAV block align".into()))?;
    let data_size = u32::try_from(pcm.len())
        .map_err(|_| AppError::BadRequest("PCM data is too large for WAV".into()))?;
    let riff_size = 36u32
        .checked_add(data_size)
        .ok_or_else(|| AppError::BadRequest("WAV data is too large".into()))?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(sample_width * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

pub(super) async fn minimax_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_minimax_audio_credential(store, provider)?;
    let format = tts_response_format(payload)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "speech-02-hd"
            } else {
                configured
            }
        });
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("English_expressive_narrator");
    let url = minimax_tts_url(&credential.base_url, payload)?;
    let t2a_v2 = url.as_str().contains("t2a_v2");
    let body = if t2a_v2 {
        json!({
            "model": model,
            "text": text,
            "voice_setting": {
                "voice_id": voice,
                "speed": payload.get("speed").and_then(Value::as_f64).unwrap_or(1.0),
                "vol": payload.get("vol").or_else(|| payload.get("volume")).and_then(Value::as_f64).unwrap_or(1.0),
                "pitch": payload.get("pitch").and_then(Value::as_i64).unwrap_or(0),
                "emotion": payload.get("emotion").and_then(Value::as_str).unwrap_or("neutral")
            },
            "audio_setting": {
                "sample_rate": payload.get("sampleRate").or_else(|| payload.get("sample_rate")).and_then(Value::as_u64).unwrap_or(32000),
                "bitrate": payload.get("bitrate").and_then(Value::as_u64).unwrap_or(128000),
                "format": if format == "opus" { "mp3" } else { format.as_str() },
                "channel": payload.get("channel").and_then(Value::as_u64).unwrap_or(1)
            }
        })
    } else {
        json!({
            "model": model,
            "text": text,
            "voice_id": voice
        })
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build MiniMax TTS client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .bearer_auth(&credential.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("MiniMax text_to_speech failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read MiniMax TTS response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "MiniMax text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let audio = if t2a_v2 || content_type.contains("json") || bytes.first() == Some(&b'{') {
        decode_minimax_tts_json_response(&bytes)?
    } else {
        bytes.to_vec()
    };
    if audio.is_empty() {
        return Err(AppError::BadRequest(
            "MiniMax text_to_speech returned empty audio".into(),
        ));
    }
    let path = store.save_tool_binary_artifact(run_id, "text_to_speech", &format, &audio)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "minimax",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "voice": voice,
        "format": format,
        "endpoint": if t2a_v2 { "t2a_v2" } else { "text_to_speech" },
        "artifact": {
            "path": path.to_string_lossy(),
            "sizeBytes": audio.len()
        }
    }))?)
}

#[derive(Clone, Debug)]
struct MiniMaxAudioCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_minimax_audio_credential(
    store: &AppStore,
    provider: &LlmProvider,
) -> AppResult<MiniMaxAudioCredential> {
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        return Ok(MiniMaxAudioCredential {
            api_key,
            base_url: minimax_audio_base_url(provider, None),
            source: if provider.api_key.is_some() {
                format!("provider:{}", provider.id)
            } else {
                format!("env:{}", provider.api_key_env)
            },
        });
    }
    if let Some(api_key) = std::env::var("MINIMAX_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(MiniMaxAudioCredential {
            api_key,
            base_url: minimax_audio_base_url(provider, None),
            source: "env:MINIMAX_API_KEY".into(),
        });
    }
    let config = store.config()?;
    if let Some(api_key) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| env.get("MINIMAX_API_KEY"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let dashboard_base = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| {
                env.get("MINIMAX_TTS_BASE_URL")
                    .or_else(|| env.get("MINIMAX_BASE_URL"))
                    .and_then(Value::as_str)
            });
        return Ok(MiniMaxAudioCredential {
            api_key,
            base_url: minimax_audio_base_url(provider, dashboard_base),
            source: "dashboardEnv:MINIMAX_API_KEY".into(),
        });
    }
    Err(AppError::BadRequest(
        "MINIMAX_API_KEY is not set for MiniMax TTS".into(),
    ))
}

fn minimax_audio_base_url(provider: &LlmProvider, dashboard_base_url: Option<&str>) -> String {
    let provider_base = provider.base_url.trim();
    if !provider_base.is_empty() {
        return provider_base.trim_end_matches('/').to_string();
    }
    std::env::var("MINIMAX_TTS_BASE_URL")
        .ok()
        .or_else(|| std::env::var("MINIMAX_BASE_URL").ok())
        .or_else(|| dashboard_base_url.map(str::to_string))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.minimax.io/v1/t2a_v2".into())
}

pub(super) fn minimax_tts_url(base_url: &str, payload: &Value) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid MiniMax TTS provider URL: {error}"))
    })?;
    if let Some(group_id) = payload
        .get("groupId")
        .or_else(|| payload.get("group_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("MINIMAX_GROUP_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    {
        let has_group = url
            .query_pairs()
            .any(|(key, _)| key.eq_ignore_ascii_case("GroupId"));
        if !has_group {
            url.query_pairs_mut().append_pair("GroupId", &group_id);
        }
    }
    Ok(url)
}

pub(super) fn decode_minimax_tts_json_response(bytes: &[u8]) -> AppResult<Vec<u8>> {
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| AppError::BadRequest(format!("invalid MiniMax TTS JSON: {error}")))?;
    if let Some(base_resp) = value.get("base_resp") {
        let status_code = base_resp
            .get("status_code")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        if status_code != 0 {
            let message = base_resp
                .get("status_msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(AppError::BadRequest(format!(
                "MiniMax TTS API error (code {status_code}): {message}"
            )));
        }
    }
    let hex_audio = value
        .get("data")
        .and_then(|data| data.get("audio"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("MiniMax TTS returned empty audio data".into()))?;
    decode_hex_audio(hex_audio)
}

fn decode_hex_audio(hex_audio: &str) -> AppResult<Vec<u8>> {
    let compact = hex_audio.trim();
    if compact.len() % 2 != 0 {
        return Err(AppError::BadRequest(
            "MiniMax TTS audio hex has odd length".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(compact.len() / 2);
    let chars = compact.as_bytes();
    for index in (0..chars.len()).step_by(2) {
        let pair = std::str::from_utf8(&chars[index..index + 2])
            .map_err(|error| AppError::BadRequest(format!("invalid MiniMax audio hex: {error}")))?;
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|error| AppError::BadRequest(format!("invalid MiniMax audio hex: {error}")))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

pub(super) async fn elevenlabs_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_elevenlabs_audio_credential(store, provider)?;
    let requested_format = tts_response_format(payload)?;
    let output_format = elevenlabs_output_format(payload, &requested_format);
    let artifact_format = if output_format.starts_with("opus") {
        "opus"
    } else {
        "mp3"
    };
    let model = payload
        .get("model")
        .or_else(|| payload.get("modelId"))
        .or_else(|| payload.get("model_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "eleven_multilingual_v2"
            } else {
                configured
            }
        });
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("pNInz6obpgDQGcFmaJgB");
    let mut url = elevenlabs_text_to_speech_url(&credential.base_url, voice)?;
    url.query_pairs_mut()
        .append_pair("output_format", &output_format);
    let mut body = json!({
        "text": text,
        "model_id": model,
    });
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build ElevenLabs TTS client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .header("xi-api-key", &credential.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("ElevenLabs text_to_speech failed: {error}"))
        })?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read ElevenLabs TTS response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "ElevenLabs text_to_speech returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let audio = if content_type.contains("json") || bytes.first() == Some(&b'{') {
        decode_tts_json_response(&bytes)?
    } else {
        bytes.to_vec()
    };
    if audio.is_empty() {
        return Err(AppError::BadRequest(
            "ElevenLabs text_to_speech returned empty audio".into(),
        ));
    }
    let path =
        store.save_tool_binary_artifact(run_id, "text_to_speech", artifact_format, &audio)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "elevenlabs",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "voice": voice,
        "format": artifact_format,
        "outputFormat": output_format,
        "artifact": {
            "path": path.to_string_lossy(),
            "sizeBytes": audio.len()
        }
    }))?)
}

fn elevenlabs_output_format(payload: &Value, requested_format: &str) -> String {
    if let Some(format) = payload
        .get("outputFormat")
        .or_else(|| payload.get("output_format"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return format.to_string();
    }
    if requested_format == "opus" {
        "opus_48000_64".into()
    } else {
        "mp3_44100_128".into()
    }
}

pub(super) fn elevenlabs_text_to_speech_url(
    base_url: &str,
    voice_id: &str,
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid ElevenLabs TTS provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.contains("/text-to-speech/") {
        return Ok(url);
    }
    let path = if path.ends_with("/speech-to-text") {
        path.trim_end_matches("/speech-to-text").to_string()
    } else if path.ends_with("/voices") {
        path.trim_end_matches("/voices").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/text-to-speech/");
    next.push_str(voice_id.trim_matches('/'));
    url.set_path(&next);
    Ok(url)
}

pub(super) fn edge_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let format = tts_response_format(payload)?;
    let command_template = payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let command = provider.base_url.trim();
            (!command.is_empty()).then(|| command.to_string())
        })
        .or_else(|| std::env::var("HERMES_EDGE_TTS_COMMAND").ok())
        .or_else(|| std::env::var("SYNTHCHAT_EDGE_TTS_COMMAND").ok());
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("zh-CN-XiaoxiaoNeural");
    let speed = payload
        .get("speed")
        .map(|value| {
            value
                .as_f64()
                .map(|number| number.to_string())
                .or_else(|| value.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "1.0".into());
    let rate = edge_tts_rate_from_speed(&speed)?;
    let volume = payload
        .get("volume")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("+0%");
    let pitch = payload
        .get("pitch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("+0Hz");
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(provider.timeout_seconds.max(1).max(60))
        .max(1);
    let temp_dir = std::env::temp_dir().join(format!("synthchat-edge-tts-{}", timestamp_millis()?));
    fs::create_dir_all(&temp_dir)?;
    let input_path = temp_dir.join("input.txt");
    let output_format = if command_template.is_none() {
        "mp3"
    } else {
        &format
    };
    let output_path = temp_dir.join(format!("output.{output_format}"));
    fs::write(&input_path, text)?;
    let command_text = command_template
        .map(|template| {
            render_edge_tts_command(
                &template,
                &input_path,
                &output_path,
                &format,
                voice,
                &rate,
                volume,
                pitch,
            )
        })
        .unwrap_or_else(|| default_edge_tts_command(&input_path, &output_path, voice, &rate, volume, pitch));
    let result = (|| {
        let _ = run_shell_command_with_timeout(&command_text, timeout_seconds)?;
        let audio = fs::read(&output_path).map_err(|error| {
            AppError::BadRequest(format!(
                "Edge TTS produced no output at {}: {error}",
                output_path.display()
            ))
        })?;
        if audio.is_empty() {
            return Err(AppError::BadRequest(format!(
                "Edge TTS produced empty output at {}",
                output_path.display()
            )));
        }
        let artifact = finalize_tts_audio(store, run_id, "edge", &audio, output_format, &format)?;
        Ok::<_, AppError>(serde_json::to_string_pretty(&json!({
            "provider": "edge",
            "providerId": provider.id,
            "voice": voice,
            "rate": rate,
            "volume": volume,
            "pitch": pitch,
            "format": format,
            "actualFormat": artifact.format,
            "voiceCompatible": artifact.voice_compatible,
            "voice_compatible": artifact.voice_compatible,
            "mediaTag": tts_media_tag(&artifact),
            "media_tag": tts_media_tag(&artifact),
            "conversion": artifact.conversion,
            "source": output_path.to_string_lossy(),
            "artifact": {
                "path": artifact.path.to_string_lossy(),
                "sizeBytes": artifact.size
            }
        }))?)
    })();
    let _ = fs::remove_dir_all(temp_dir);
    result
}

pub(super) fn edge_tts_rate_from_speed(speed: &str) -> AppResult<String> {
    let speed = speed.trim();
    if speed.is_empty() {
        return Ok("+0%".into());
    }
    if speed.ends_with('%') {
        return Ok(speed.to_string());
    }
    let value = speed.parse::<f64>().map_err(|_| {
        AppError::BadRequest("Edge TTS speed must be a number or rate percent".into())
    })?;
    if !(0.25..=4.0).contains(&value) {
        return Err(AppError::BadRequest(
            "Edge TTS speed must be between 0.25 and 4.0".into(),
        ));
    }
    let pct = ((value - 1.0) * 100.0).round() as i64;
    Ok(format!("{pct:+}%"))
}

pub(super) fn default_edge_tts_command(
    input_path: &Path,
    output_path: &Path,
    voice: &str,
    rate: &str,
    volume: &str,
    pitch: &str,
) -> String {
    format!(
        "python -m edge_tts --file {} --voice {} --rate={} --volume={} --pitch={} --write-media {}",
        shell_quote_path(input_path),
        shell_quote_value(voice),
        shell_quote_value(rate),
        shell_quote_value(volume),
        shell_quote_value(pitch),
        shell_quote_path(output_path)
    )
}

fn render_edge_tts_command(
    template: &str,
    input_path: &Path,
    output_path: &Path,
    format: &str,
    voice: &str,
    rate: &str,
    volume: &str,
    pitch: &str,
) -> String {
    render_local_tts_command(template, input_path, output_path, format, voice, "edge", "")
        .replace("{rate}", &shell_quote_value(rate))
        .replace("{volume}", &shell_quote_value(volume))
        .replace("{pitch}", &shell_quote_value(pitch))
}

pub(super) fn local_python_engine_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let engine = local_python_tts_engine(provider)?;
    let format = tts_response_format(payload)?;
    let command_template = payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let command = provider.base_url.trim();
            (!command.is_empty()).then(|| command.to_string())
        })
        .or_else(|| local_python_tts_env_command(engine));
    let model = local_python_tts_model(engine, provider, payload);
    let voice = local_python_tts_voice(engine, payload);
    let speed = payload
        .get("speed")
        .map(|value| {
            value
                .as_f64()
                .map(|number| number.to_string())
                .or_else(|| value.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "1.0".into());
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(provider.timeout_seconds.max(1).max(120))
        .max(1);
    let temp_dir =
        std::env::temp_dir().join(format!("synthchat-{}-tts-{}", engine, timestamp_millis()?));
    fs::create_dir_all(&temp_dir)?;
    let input_path = temp_dir.join("input.txt");
    let output_path = temp_dir.join(format!("output.{format}"));
    fs::write(&input_path, text)?;
    let command_text = command_template
        .map(|template| {
            render_local_tts_command(
                &template,
                &input_path,
                &output_path,
                &format,
                &voice,
                &model,
                &speed,
            )
        })
        .unwrap_or_else(|| {
            default_local_python_tts_command(
                engine,
                &input_path,
                &output_path,
                &model,
                &voice,
                &speed,
                payload,
            )
        });
    let result = run_local_tts_command_artifact(
        store,
        run_id,
        engine,
        &provider.id,
        Some(&model),
        Some(&voice),
        Some(&speed),
        &format,
        &output_path,
        &command_text,
        timeout_seconds,
    );
    let _ = fs::remove_dir_all(temp_dir);
    result
}

fn local_python_tts_engine(provider: &LlmProvider) -> AppResult<&'static str> {
    match provider.provider_type.trim().to_ascii_lowercase().as_str() {
        "piper" => Ok("piper"),
        "kittentts" | "kitten_tts" | "kitten-tts" => Ok("kittentts"),
        "neutts" | "neu_tts" | "neu-tts" => Ok("neutts"),
        other => Err(AppError::BadRequest(format!(
            "unsupported local Python TTS provider type: {other}"
        ))),
    }
}

fn local_python_tts_env_command(engine: &str) -> Option<String> {
    match engine {
        "piper" => std::env::var("HERMES_PIPER_TTS_COMMAND")
            .ok()
            .or_else(|| std::env::var("SYNTHCHAT_PIPER_TTS_COMMAND").ok()),
        "kittentts" => std::env::var("HERMES_KITTENTTS_COMMAND")
            .ok()
            .or_else(|| std::env::var("SYNTHCHAT_KITTENTTS_COMMAND").ok()),
        "neutts" => std::env::var("HERMES_NEUTTS_COMMAND")
            .ok()
            .or_else(|| std::env::var("SYNTHCHAT_NEUTTS_COMMAND").ok()),
        _ => None,
    }
}

fn local_python_tts_model(engine: &str, provider: &LlmProvider, payload: &Value) -> String {
    if let Some(model) = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return model.to_string();
    }
    let configured = provider.model.trim();
    if !configured.is_empty() && configured != "echo" {
        return configured.to_string();
    }
    match engine {
        "piper" => "en_US-lessac-medium".into(),
        "kittentts" => "KittenML/kitten-tts-nano-0.8-int8".into(),
        "neutts" => "neuphonic/neutts-air-q4-gguf".into(),
        _ => String::new(),
    }
}

fn local_python_tts_voice(engine: &str, payload: &Value) -> String {
    if let Some(voice) = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return voice.to_string();
    }
    match engine {
        "piper" => "en_US-lessac-medium".into(),
        "kittentts" => "Jasper".into(),
        "neutts" => "default".into(),
        _ => String::new(),
    }
}

fn default_local_python_tts_command(
    engine: &str,
    input_path: &Path,
    output_path: &Path,
    model: &str,
    voice: &str,
    speed: &str,
    payload: &Value,
) -> String {
    match engine {
        "piper" => default_piper_tts_command(input_path, output_path, model, voice),
        "kittentts" => default_kittentts_command(
            input_path,
            output_path,
            model,
            voice,
            speed,
            payload
                .get("cleanText")
                .or_else(|| payload.get("clean_text"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
        ),
        "neutts" => default_neutts_command(input_path, output_path, model, payload),
        _ => String::new(),
    }
}

pub(super) fn default_piper_tts_command(
    input_path: &Path,
    output_path: &Path,
    model: &str,
    voice: &str,
) -> String {
    let selected_voice = if model.trim().is_empty() {
        voice.trim()
    } else {
        model.trim()
    };
    let selected_voice = if selected_voice.is_empty() {
        "en_US-lessac-medium"
    } else {
        selected_voice
    };
    let voices_dir = resolve_piper_voices_dir();
    if let Some(model_path) = resolve_piper_existing_voice_path(selected_voice, &voices_dir) {
        return format!(
            "python -m piper --model {} --output_file {} < {}",
            shell_quote_path(&model_path),
            shell_quote_path(output_path),
            shell_quote_path(input_path)
        );
    }
    let script = format!(
        "from pathlib import Path; import subprocess, sys; voice={}; voices_dir=Path({}).expanduser(); voices_dir.mkdir(parents=True, exist_ok=True); candidate=Path(voice).expanduser(); cached=voices_dir / f'{{voice}}.onnx'; config=voices_dir / f'{{voice}}.onnx.json'; model=str(candidate) if candidate.suffix.lower()=='.onnx' and candidate.exists() else str(cached); subprocess.run([sys.executable, '-m', 'piper.download_voices', voice, '--download-dir', str(voices_dir)], check=True, timeout=300) if not (candidate.suffix.lower()=='.onnx' and candidate.exists()) and not (cached.exists() and config.exists()) else None; input_file=open({}, 'r', encoding='utf-8'); subprocess.run([sys.executable, '-m', 'piper', '--model', model, '--output_file', {}], stdin=input_file, check=True); input_file.close()",
        python_string_literal(selected_voice),
        python_string_literal(&voices_dir.to_string_lossy()),
        python_string_literal(&input_path.to_string_lossy()),
        python_string_literal(&output_path.to_string_lossy())
    );
    format!("python -c {}", shell_quote_value(&script))
}

pub(super) fn resolve_piper_voices_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("HERMES_PIPER_VOICES_DIR")
        .or_else(|| std::env::var_os("SYNTHCHAT_PIPER_VOICES_DIR"))
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(value);
    }
    if let Some(value) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(value).join("cache").join("piper-voices");
    }
    if let Some(value) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(value)
            .join(".hermes")
            .join("cache")
            .join("piper-voices");
    }
    PathBuf::from(".hermes").join("cache").join("piper-voices")
}

pub(super) fn resolve_piper_existing_voice_path(voice: &str, voices_dir: &Path) -> Option<PathBuf> {
    let voice = voice.trim();
    if voice.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(voice);
    if candidate
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
        && candidate.exists()
    {
        return Some(candidate.canonicalize().unwrap_or(candidate));
    }
    let cached = voices_dir.join(format!("{voice}.onnx"));
    let cached_config = voices_dir.join(format!("{voice}.onnx.json"));
    if cached.exists() && cached_config.exists() {
        return Some(cached.canonicalize().unwrap_or(cached));
    }
    None
}

pub(super) fn default_kittentts_command(
    input_path: &Path,
    output_path: &Path,
    model: &str,
    voice: &str,
    speed: &str,
    clean_text: bool,
) -> String {
    let script = format!(
        "from pathlib import Path; from kittentts import KittenTTS; import soundfile as sf; text=Path({}).read_text(encoding='utf-8'); model=KittenTTS({}); audio=model.generate(text, voice={}, speed=float({}), clean_text={}); sf.write({}, audio, 24000)",
        python_string_literal(&input_path.to_string_lossy()),
        python_string_literal(model),
        python_string_literal(voice),
        python_string_literal(speed),
        if clean_text { "True" } else { "False" },
        python_string_literal(&output_path.to_string_lossy())
    );
    format!("python -c {}", shell_quote_value(&script))
}

pub(super) fn default_neutts_command(
    input_path: &Path,
    output_path: &Path,
    model: &str,
    payload: &Value,
) -> String {
    let script = payload
        .get("script")
        .or_else(|| payload.get("scriptPath"))
        .or_else(|| payload.get("script_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("HERMES_NEUTTS_SYNTH_SCRIPT").ok())
        .or_else(|| std::env::var("SYNTHCHAT_NEUTTS_SYNTH_SCRIPT").ok())
        .unwrap_or_else(|| resolve_neutts_asset_path("neutts_synth.py"));
    let ref_audio = payload
        .get("refAudio")
        .or_else(|| payload.get("ref_audio"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolve_neutts_asset_path("neutts_samples/jo.wav"));
    let ref_text = payload
        .get("refText")
        .or_else(|| payload.get("ref_text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolve_neutts_asset_path("neutts_samples/jo.txt"));
    let device = payload
        .get("device")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("cpu");
    let text = fs::read_to_string(input_path).unwrap_or_default();
    format!(
        "python {} --text {} --out {} --ref-audio {} --ref-text {} --model {} --device {}",
        shell_quote_value(&script),
        shell_quote_value(&text),
        shell_quote_path(output_path),
        shell_quote_value(&ref_audio),
        shell_quote_value(&ref_text),
        shell_quote_value(model),
        shell_quote_value(device)
    )
}

fn resolve_neutts_asset_path(relative: &str) -> String {
    let relative = relative.trim().trim_start_matches(['/', '\\']);
    for tools_dir in hermes_agent_tools_dir_candidates() {
        let candidate = tools_dir.join(relative);
        if candidate.exists() {
            return candidate
                .canonicalize()
                .unwrap_or(candidate)
                .to_string_lossy()
                .to_string();
        }
    }
    format!("tools/{}", relative.replace('\\', "/"))
}

fn hermes_agent_tools_dir_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in [
        "HERMES_AGENT_TOOLS_DIR",
        "SYNTHCHAT_HERMES_AGENT_TOOLS_DIR",
        "HERMES_AGENT_REPO",
        "HERMES_AGENT_HOME",
    ] {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            if path.file_name().and_then(|name| name.to_str()) == Some("tools") {
                roots.push(path);
            } else {
                roots.push(path.join("tools"));
            }
        }
    }
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home).join("tools"));
    }
    if let Ok(current_dir) = std::env::current_dir() {
        roots.push(current_dir.join("tools"));
        roots.push(current_dir.join("..").join("hermes-agent").join("tools"));
        roots.push(
            current_dir
                .join("..")
                .join("..")
                .join("hermes-agent")
                .join("tools"),
        );
    }
    roots.push(PathBuf::from(
        "D:\\pro_sunner\\demo_vscode\\hermes-agent\\tools",
    ));

    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|path| {
            let normalized = path.to_string_lossy().to_ascii_lowercase();
            seen.insert(normalized).then_some(path)
        })
        .collect()
}

fn python_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

struct TtsAudioArtifact {
    path: PathBuf,
    size: usize,
    format: String,
    voice_compatible: bool,
    conversion: Value,
}

fn finalize_tts_audio(
    store: &AppStore,
    run_id: &str,
    provider: &str,
    audio: &[u8],
    source_format: &str,
    requested_format: &str,
) -> AppResult<TtsAudioArtifact> {
    let source_format = normalize_tts_audio_format(source_format);
    let requested_format = normalize_tts_audio_format(requested_format);
    if requested_format == "silk" && source_format != "silk" {
        let converted = convert_tts_audio_to_silk(provider, audio, &source_format)?;
        let path = store.save_tool_binary_artifact(run_id, "text_to_speech", "silk", &converted)?;
        return Ok(TtsAudioArtifact {
            path,
            size: converted.len(),
            voice_compatible: true,
            format: "silk".into(),
            conversion: json!({
                "performed": true,
                "from": source_format,
                "to": "silk",
                "tool": "graiax.silkcoder",
                "tencent": true
            }),
        });
    }
    if source_format == requested_format {
        if requested_format == "silk" {
            ensure_tencent_silk_audio(audio)?;
        }
        let path =
            store.save_tool_binary_artifact(run_id, "text_to_speech", &requested_format, audio)?;
        return Ok(TtsAudioArtifact {
            path,
            size: audio.len(),
            voice_compatible: tts_format_is_voice_compatible(&requested_format),
            format: requested_format,
            conversion: json!({
                "performed": false,
                "reason": "source format already matches requested format"
            }),
        });
    }
    match convert_tts_audio_with_ffmpeg(provider, audio, &source_format, &requested_format) {
        Ok(converted) => {
            let path = store.save_tool_binary_artifact(
                run_id,
                "text_to_speech",
                &requested_format,
                &converted,
            )?;
            Ok(TtsAudioArtifact {
                path,
                size: converted.len(),
                voice_compatible: tts_format_is_voice_compatible(&requested_format),
                format: requested_format.clone(),
                conversion: json!({
                    "performed": true,
                    "from": source_format,
                    "to": requested_format,
                    "tool": "ffmpeg"
                }),
            })
        }
        Err(error) => {
            if requested_format == "silk" {
                return Err(AppError::BadRequest(format!(
                    "failed to convert TTS audio from {source_format} to silk: {error}"
                )));
            }
            let path =
                store.save_tool_binary_artifact(run_id, "text_to_speech", &source_format, audio)?;
            Ok(TtsAudioArtifact {
                path,
                size: audio.len(),
                voice_compatible: tts_format_is_voice_compatible(&source_format),
                format: source_format.clone(),
                conversion: json!({
                    "performed": false,
                    "requested": requested_format,
                    "actual": source_format,
                    "reason": error.to_string()
                }),
            })
        }
    }
}

pub(super) fn ensure_tencent_silk_audio(audio: &[u8]) -> AppResult<()> {
    let header_window = &audio[..audio.len().min(16)];
    if header_window
        .windows(b"#!SILK_V3".len())
        .any(|window| window == b"#!SILK_V3")
    {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "SILK audio is missing #!SILK_V3 Tencent SILK header".into(),
        ))
    }
}

fn convert_tts_audio_to_silk(
    provider: &str,
    audio: &[u8],
    source_format: &str,
) -> AppResult<Vec<u8>> {
    let ffmpeg_available = command_available("ffmpeg");
    if !ffmpeg_available && source_format != "wav" {
        return Err(AppError::BadRequest(
            "ffmpeg not found; cannot prepare audio for SILK encoding".into(),
        ));
    }
    let temp_dir = std::env::temp_dir().join(format!(
        "synthchat-silk-{}-{}",
        provider,
        timestamp_millis()?
    ));
    fs::create_dir_all(&temp_dir)?;
    let input_path = temp_dir.join(format!("input.{source_format}"));
    let wav_path = temp_dir.join("input-16000-mono.wav");
    let silk_path = temp_dir.join("output.silk");
    fs::write(&input_path, audio)?;

    let result = (|| {
        if ffmpeg_available {
            let ffmpeg_command = format!(
                "ffmpeg -y -loglevel error -i {} -ar 16000 -ac 1 -acodec pcm_s16le {}",
                shell_quote_path(&input_path),
                shell_quote_path(&wav_path)
            );
            let _ = run_shell_command_with_timeout(&ffmpeg_command, 30)?;
        } else if source_format == "wav" {
            fs::copy(&input_path, &wav_path).map_err(|error| {
                AppError::BadRequest(format!("failed to stage WAV for SILK encoding: {error}"))
            })?;
        }
        encode_wav_to_tencent_silk(&wav_path, &silk_path)?;
        let converted = fs::read(&silk_path).map_err(|error| {
            AppError::BadRequest(format!("SILK encoder did not produce output: {error}"))
        })?;
        if converted.is_empty() {
            return Err(AppError::BadRequest(
                "SILK encoder produced empty output".into(),
            ));
        }
        ensure_tencent_silk_audio(&converted)?;
        Ok(converted)
    })();
    let _ = fs::remove_dir_all(temp_dir);
    result
}

fn encode_wav_to_tencent_silk(wav_path: &Path, silk_path: &Path) -> AppResult<()> {
    let python = std::env::var("SYNTHCHAT_TTS_PYTHON")
        .ok()
        .or_else(|| std::env::var("SYNTHCHAT_CHATTTS_PYTHON").ok())
        .or_else(|| std::env::var("HERMES_TTS_PYTHON").ok())
        .or_else(|| std::env::var("HERMES_CHATTTS_PYTHON").ok())
        .unwrap_or_else(default_desktop_python_command);
    let script_path = silk_path.with_file_name("encode_silk.py");
    fs::write(
        &script_path,
        "import sys\nfrom graiax import silkcoder\nsilkcoder.encode(sys.argv[1], sys.argv[2], rate=16000, tencent=True)\n",
    )?;
    let command_text = format!(
        "{} {} {} {}",
        shell_quote_value(&python),
        shell_quote_path(&script_path),
        shell_quote_path(wav_path),
        shell_quote_path(silk_path)
    );
    let _ = run_shell_command_with_timeout(&command_text, 30).map_err(|error| {
        AppError::BadRequest(format!(
            "graiax.silkcoder failed; install graiax-silkcoder in the configured TTS Python environment: {error}"
        ))
    })?;
    Ok(())
}

fn tts_format_is_voice_compatible(format: &str) -> bool {
    matches!(
        normalize_tts_audio_format(format).as_str(),
        "opus" | "mp3" | "wav" | "silk"
    )
}

fn tts_media_tag(artifact: &TtsAudioArtifact) -> String {
    let media = format!("MEDIA:{}", artifact.path.to_string_lossy());
    if artifact.voice_compatible {
        format!("[[audio_as_voice]]\n{media}")
    } else {
        media
    }
}

fn normalize_tts_audio_format(format: &str) -> String {
    match format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "ogg" | "opus" => "opus".into(),
        "wave" | "wav" => "wav".into(),
        "mpeg" | "mp3" => "mp3".into(),
        "silk" => "silk".into(),
        other if other.is_empty() => "mp3".into(),
        other => other.to_string(),
    }
}

fn tts_audio_mime(format: &str) -> &'static str {
    match normalize_tts_audio_format(format).as_str() {
        "wav" => "audio/wav",
        "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "silk" => "audio/silk",
        _ => "application/octet-stream",
    }
}

fn convert_tts_audio_with_ffmpeg(
    provider: &str,
    audio: &[u8],
    source_format: &str,
    requested_format: &str,
) -> AppResult<Vec<u8>> {
    if !command_available("ffmpeg") {
        return Err(AppError::BadRequest(
            "ffmpeg not found; saved original audio format".into(),
        ));
    }
    let temp_dir = std::env::temp_dir().join(format!(
        "synthchat-ffmpeg-{}-{}",
        provider,
        timestamp_millis()?
    ));
    fs::create_dir_all(&temp_dir)?;
    let input_path = temp_dir.join(format!("input.{source_format}"));
    let output_ext = if requested_format == "opus" {
        "ogg"
    } else {
        requested_format
    };
    let output_path = temp_dir.join(format!("output.{output_ext}"));
    fs::write(&input_path, audio)?;
    let command_text = if requested_format == "opus" {
        format!(
            "ffmpeg -y -loglevel error -i {} -acodec libopus {}",
            shell_quote_path(&input_path),
            shell_quote_path(&output_path)
        )
    } else {
        format!(
            "ffmpeg -y -loglevel error -i {} {}",
            shell_quote_path(&input_path),
            shell_quote_path(&output_path)
        )
    };
    let result = (|| {
        let _ = run_shell_command_with_timeout(&command_text, 30)?;
        let converted = fs::read(&output_path).map_err(|error| {
            AppError::BadRequest(format!("ffmpeg did not produce converted audio: {error}"))
        })?;
        if converted.is_empty() {
            return Err(AppError::BadRequest(
                "ffmpeg produced empty converted audio".into(),
            ));
        }
        Ok(converted)
    })();
    let _ = fs::remove_dir_all(temp_dir);
    result
}

fn run_local_tts_command_artifact(
    store: &AppStore,
    run_id: &str,
    provider: &str,
    provider_id: &str,
    model: Option<&str>,
    voice: Option<&str>,
    speed: Option<&str>,
    format: &str,
    output_path: &Path,
    command_text: &str,
    timeout_seconds: u64,
) -> AppResult<String> {
    let _ = run_shell_command_with_timeout(command_text, timeout_seconds)?;
    let mut actual_output_path = output_path.to_path_buf();
    let mut source_format = format.to_string();
    if !actual_output_path.exists() {
        let wav_path = output_path.with_extension("wav");
        if wav_path.exists() {
            actual_output_path = wav_path;
            source_format = "wav".into();
        }
    }
    let audio = fs::read(&actual_output_path).map_err(|error| {
        AppError::BadRequest(format!(
            "{provider} TTS produced no output at {}: {error}",
            actual_output_path.display()
        ))
    })?;
    if audio.is_empty() {
        return Err(AppError::BadRequest(format!(
            "{provider} TTS produced empty output at {}",
            actual_output_path.display()
        )));
    }
    let artifact = finalize_tts_audio(store, run_id, provider, &audio, &source_format, format)?;
    Ok(serde_json::to_string_pretty(&json!({
        "provider": provider,
        "providerId": provider_id,
        "model": model.unwrap_or(""),
        "voice": voice.unwrap_or(""),
        "speed": speed.unwrap_or(""),
        "format": format,
        "actualFormat": artifact.format,
        "voiceCompatible": artifact.voice_compatible,
        "voice_compatible": artifact.voice_compatible,
        "mediaTag": tts_media_tag(&artifact),
        "media_tag": tts_media_tag(&artifact),
        "conversion": artifact.conversion,
        "source": actual_output_path.to_string_lossy(),
        "artifact": {
            "path": artifact.path.to_string_lossy(),
            "sizeBytes": artifact.size
        }
    }))?)
}

pub(super) fn local_command_text_to_speech(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    text: &str,
    payload: &Value,
) -> AppResult<String> {
    let command_template = payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let command = provider.base_url.trim();
            (!command.is_empty()).then(|| command.to_string())
        })
        .or_else(|| std::env::var("HERMES_LOCAL_TTS_COMMAND").ok())
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_TTS_COMMAND").ok())
        .ok_or_else(|| {
            AppError::BadRequest(
                "local_command TTS requires payload.command, provider.base_url, or HERMES_LOCAL_TTS_COMMAND"
                    .into(),
            )
        })?;
    let format = tts_response_format(payload)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| provider.model.trim());
    let voice = payload
        .get("voice")
        .or_else(|| payload.get("voiceId"))
        .or_else(|| payload.get("voice_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    let speed = payload
        .get("speed")
        .map(|value| {
            value
                .as_f64()
                .map(|number| number.to_string())
                .or_else(|| value.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(provider.timeout_seconds.max(1))
        .max(1);
    let temp_dir = std::env::temp_dir().join(format!("synthchat-tts-{}", timestamp_millis()?));
    fs::create_dir_all(&temp_dir)?;
    let input_path = temp_dir.join("input.txt");
    let output_path = temp_dir.join(format!("output.{format}"));
    fs::write(&input_path, text)?;
    let command_text = render_local_tts_command(
        &command_template,
        &input_path,
        &output_path,
        &format,
        voice,
        model,
        &speed,
    );
    let result = (|| {
        let _ = run_shell_command_with_timeout(&command_text, timeout_seconds)?;
        let audio = fs::read(&output_path).map_err(|error| {
            AppError::BadRequest(format!(
                "local_command TTS produced no output at {}: {error}",
                output_path.display()
            ))
        })?;
        if audio.is_empty() {
            return Err(AppError::BadRequest(format!(
                "local_command TTS produced empty output at {}",
                output_path.display()
            )));
        }
        let artifact =
            finalize_tts_audio(store, run_id, "local_command", &audio, &format, &format)?;
        Ok::<_, AppError>(serde_json::to_string_pretty(&json!({
            "provider": "local_command",
            "providerId": provider.id,
            "model": model,
            "voice": voice,
            "speed": speed,
            "format": format,
            "actualFormat": artifact.format,
            "voiceCompatible": artifact.voice_compatible,
            "voice_compatible": artifact.voice_compatible,
            "mediaTag": tts_media_tag(&artifact),
            "media_tag": tts_media_tag(&artifact),
            "conversion": artifact.conversion,
            "source": output_path.to_string_lossy(),
            "artifact": {
                "path": artifact.path.to_string_lossy(),
                "sizeBytes": artifact.size
            }
        }))?)
    })();
    let _ = fs::remove_dir_all(temp_dir);
    result
}

fn render_local_tts_command(
    template: &str,
    input_path: &Path,
    output_path: &Path,
    format: &str,
    voice: &str,
    model: &str,
    speed: &str,
) -> String {
    let input = shell_quote_path(input_path);
    let output = shell_quote_path(output_path);
    template
        .replace("{input_path}", &input)
        .replace("{text_path}", &input)
        .replace("{output_path}", &output)
        .replace("{format}", &shell_quote_value(format))
        .replace("{voice}", &shell_quote_value(voice))
        .replace("{model}", &shell_quote_value(model))
        .replace("{speed}", &shell_quote_value(speed))
}

pub(super) fn audio_speech_url(provider: &LlmProvider) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid TTS provider URL: {error}")))?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/audio/speech") {
        return Ok(url);
    }
    let path = if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/audio/speech");
    url.set_path(&next);
    Ok(url)
}

pub(super) fn tts_response_format(payload: &Value) -> AppResult<String> {
    let format = payload
        .get("format")
        .or_else(|| payload.get("response_format"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("mp3")
        .to_lowercase();
    match format.as_str() {
        "mp3" | "opus" | "aac" | "flac" | "wav" | "pcm" | "silk" => Ok(format),
        _ => Err(AppError::BadRequest(format!(
            "unsupported text_to_speech format: {format}"
        ))),
    }
}

pub(super) fn decode_tts_json_response(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use base64::Engine;
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| AppError::BadRequest(format!("invalid TTS JSON: {error}")))?;
    let encoded = value
        .get("audio")
        .or_else(|| value.get("b64_json"))
        .or_else(|| value.get("data"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("TTS JSON response missing audio data".into()))?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| AppError::BadRequest(format!("invalid TTS audio base64: {error}")))
}

const MAX_TRANSCRIBE_AUDIO_BYTES: usize = 25 * 1024 * 1024;

pub(super) async fn transcribe_audio_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let source = string_arg(
        payload,
        &[
            "path",
            "audioPath",
            "audio_path",
            "url",
            "audioUrl",
            "audio_url",
            "source",
        ],
    )
    .ok_or_else(|| {
        AppError::BadRequest("transcribe_audio requires payload.path or payload.url".into())
    })?;
    let provider = match payload
        .get("providerId")
        .or_else(|| payload.get("provider_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(provider_id) => store.provider(Some(provider_id))?,
        None => store.provider(None)?,
    };
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "custom" | "" => {
            if provider.provider_type == "echo" || provider.base_url.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "transcribe_audio requires an enabled OpenAI-compatible provider".into(),
                ));
            }
            openai_compatible_transcribe_audio(store, agent, run_id, &provider, &source, payload)
                .await
        }
        "local_command" | "command_stt" | "stt-command" | "command" => {
            local_command_transcribe_audio(store, agent, run_id, &provider, &source, payload)
        }
        "xai" | "x-ai" | "grok" => {
            xai_transcribe_audio(store, agent, run_id, &provider, &source, payload).await
        }
        "elevenlabs" | "eleven_labs" | "scribe" => {
            elevenlabs_transcribe_audio(store, agent, run_id, &provider, &source, payload).await
        }
        "mistral" | "voxtral" => {
            mistral_transcribe_audio(store, agent, run_id, &provider, &source, payload).await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported transcribe_audio provider type: {other}"
        ))),
    }
}

pub(super) fn desktop_local_command_transcribe_audio(
    store: &AppStore,
    run_id: &str,
    source: &str,
    payload: &Value,
) -> AppResult<Option<String>> {
    let Some(command_template) = desktop_local_stt_command_template() else {
        return Ok(None);
    };
    let (audio_path, cleanup_path) = desktop_local_stt_source_path(source)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_MODEL").ok())
        .or_else(|| std::env::var("HERMES_LOCAL_STT_MODEL").ok())
        .unwrap_or_else(|| "small".into());
    let language = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_LANGUAGE").ok())
        .or_else(|| std::env::var("HERMES_LOCAL_STT_LANGUAGE").ok())
        .unwrap_or_else(|| "zh".into());
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("SYNTHCHAT_LOCAL_STT_TIMEOUT_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .or_else(|| {
            std::env::var("HERMES_LOCAL_STT_TIMEOUT_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(120)
        .max(1);
    let output_path =
        std::env::temp_dir().join(format!("synthchat-chat-stt-{}.txt", timestamp_millis()?));
    let command_text = render_local_stt_command(
        &command_template,
        &audio_path,
        &output_path,
        &model,
        &language,
    );
    let result = (|| {
        let output = run_shell_command_with_timeout(&command_text, timeout_seconds)?;
        let raw_transcript = read_local_stt_output(&output_path, &output)?;
        let filtered = is_whisper_hallucination(&raw_transcript);
        let transcript = if filtered {
            String::new()
        } else {
            raw_transcript.clone()
        };
        let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
        Ok::<_, AppError>(serde_json::to_string_pretty(&json!({
            "success": true,
            "provider": "desktop_local_command",
            "providerId": "desktop-local-stt",
            "model": model,
            "language": language,
            "source": audio_path.to_string_lossy(),
            "artifactPath": artifact_path.to_string_lossy(),
            "transcript": transcript,
            "filtered": filtered,
            "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
            "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
        }))?)
    })();
    let _ = fs::remove_file(&output_path);
    if let Some(path) = cleanup_path {
        let _ = fs::remove_file(path);
    }
    result.map(Some)
}

fn desktop_local_stt_command_template() -> Option<String> {
    std::env::var("HERMES_LOCAL_STT_COMMAND")
        .ok()
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_COMMAND").ok())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            python_module_available("faster_whisper").then(|| {
                let script = "from faster_whisper import WhisperModel; import pathlib, sys; audio, out, model, language = sys.argv[1:5]; whisper = WhisperModel(model or 'small', device='cpu', compute_type='int8'); segments, _ = whisper.transcribe(audio, language=(language or None), vad_filter=True); pathlib.Path(out).write_text(''.join(segment.text for segment in segments).strip(), encoding='utf-8')";
                format!(
                    "python -c {} {{path}} {{output_path}} {{model}} {{language}}",
                    shell_quote_value(script)
                )
            })
        })
        .or_else(|| {
            command_available("whisper").then(|| {
                "whisper {path} --model {model} --language {language} --output_format txt --output_dir {output_dir}".into()
            })
        })
}

fn desktop_local_stt_source_path(source: &str) -> AppResult<(PathBuf, Option<PathBuf>)> {
    let source = source.trim();
    if source.starts_with("data:audio/") {
        let (mime, bytes) = decode_audio_data_url(source)?;
        ensure_transcribe_audio_size(bytes.len())?;
        let path = std::env::temp_dir().join(format!(
            "synthchat-chat-stt-{}.{}",
            timestamp_millis()?,
            audio_extension_from_mime(&mime)
        ));
        fs::write(&path, bytes)?;
        return Ok((path.clone(), Some(path)));
    }
    if source.starts_with("file://") {
        let path = reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")));
        if path.is_file() {
            return Ok((path, None));
        }
    }
    let path = PathBuf::from(source);
    if path.is_file() {
        return Ok((path, None));
    }
    Err(AppError::BadRequest(
        "desktop local STT requires inline audio data or a local audio file".into(),
    ))
}

pub(super) async fn openai_compatible_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "whisper-1"
            } else {
                configured
            }
        });
    if let Some(path) = oversized_local_wav_for_transcription(agent, source)? {
        return openai_compatible_transcribe_wav_chunks(
            store, run_id, provider, &path, model, payload,
        )
        .await;
    }
    let (bytes, filename, mime_type, source_label) = transcribe_audio_bytes(agent, source).await?;
    let raw_transcript = openai_compatible_transcribe_audio_part(
        provider,
        bytes.clone(),
        filename.clone(),
        mime_type.clone(),
        model,
        payload,
    )
    .await?;
    let filtered = is_whisper_hallucination(&raw_transcript);
    let transcript = if filtered {
        String::new()
    } else {
        raw_transcript.clone()
    };
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": provider.provider_type,
        "providerId": provider.id,
        "model": model,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
    }))?)
}

pub(super) fn local_command_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let path = local_transcription_path(agent, source)?;
    let command_template = payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let command = provider.base_url.trim();
            (!command.is_empty()).then(|| command.to_string())
        })
        .or_else(|| std::env::var("HERMES_LOCAL_STT_COMMAND").ok())
        .or_else(|| std::env::var("SYNTHCHAT_LOCAL_STT_COMMAND").ok())
        .ok_or_else(|| {
            AppError::BadRequest(
                "local_command STT requires payload.command, provider.base_url, or HERMES_LOCAL_STT_COMMAND"
                    .into(),
            )
        })?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| provider.model.trim());
    let language = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("en");
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(provider.timeout_seconds.max(1))
        .max(1);
    let output_path =
        std::env::temp_dir().join(format!("synthchat-stt-{}.txt", timestamp_millis()?));
    let command_text =
        render_local_stt_command(&command_template, &path, &output_path, model, language);
    let output = run_shell_command_with_timeout(&command_text, timeout_seconds)?;
    let raw_transcript = read_local_stt_output(&output_path, &output)?;
    let _ = fs::remove_file(&output_path);
    let filtered = is_whisper_hallucination(&raw_transcript);
    let transcript = if filtered {
        String::new()
    } else {
        raw_transcript.clone()
    };
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": "local_command",
        "providerId": provider.id,
        "model": model,
        "language": language,
        "source": path.to_string_lossy(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
    }))?)
}

pub(super) async fn xai_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_xai_audio_credential(store, provider, "XAI_STT_BASE_URL")?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "grok-stt"
            } else {
                configured
            }
        });
    let language = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("en");
    let (bytes, filename, mime_type, source_label) = transcribe_audio_bytes(agent, source).await?;
    ensure_transcribe_audio_size(bytes.len())?;
    let raw_transcript = xai_transcribe_audio_part(
        &credential,
        provider.timeout_seconds.max(1),
        bytes.clone(),
        filename,
        mime_type.clone(),
        language,
        payload,
    )
    .await?;
    let filtered = is_whisper_hallucination(&raw_transcript);
    let transcript = if filtered {
        String::new()
    } else {
        raw_transcript.clone()
    };
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": "xai",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "language": language,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
    }))?)
}

#[derive(Clone, Debug)]
struct XaiAudioCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_xai_audio_credential(
    store: &AppStore,
    provider: &LlmProvider,
    base_env: &str,
) -> AppResult<XaiAudioCredential> {
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        return Ok(XaiAudioCredential {
            api_key,
            base_url: xai_audio_base_url(provider, base_env, None),
            source: if provider.api_key.is_some() {
                format!("provider:{}", provider.id)
            } else {
                format!("env:{}", provider.api_key_env)
            },
        });
    }

    let mut oauth_provider = LlmProvider::default();
    oauth_provider.id = "xai-oauth".into();
    oauth_provider.name = "xAI OAuth".into();
    oauth_provider.provider_type = "xai-oauth".into();
    oauth_provider.preset = Some("xai-oauth".into());
    if let Some(credential) = crate::hermes_auth::resolve_hermes_runtime_credential(&oauth_provider)
    {
        if !credential.api_key.trim().is_empty() {
            return Ok(XaiAudioCredential {
                api_key: credential.api_key,
                base_url: xai_audio_base_url(provider, base_env, credential.base_url.as_deref()),
                source: credential.source,
            });
        }
    }

    if let Some(api_key) = std::env::var("XAI_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(XaiAudioCredential {
            api_key,
            base_url: xai_audio_base_url(provider, base_env, None),
            source: "env:XAI_API_KEY".into(),
        });
    }

    let config = store.config()?;
    if let Some(api_key) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| env.get("XAI_API_KEY"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let dashboard_base = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| {
                env.get(base_env)
                    .or_else(|| env.get("XAI_BASE_URL"))
                    .and_then(Value::as_str)
            });
        return Ok(XaiAudioCredential {
            api_key,
            base_url: xai_audio_base_url(provider, base_env, dashboard_base),
            source: "dashboardEnv:XAI_API_KEY".into(),
        });
    }

    Err(AppError::BadRequest(
        "No xAI credentials found. Configure xAI OAuth or set XAI_API_KEY".into(),
    ))
}

fn xai_audio_base_url(
    provider: &LlmProvider,
    base_env: &str,
    credential_base_url: Option<&str>,
) -> String {
    let provider_base = provider.base_url.trim();
    if !provider_base.is_empty() {
        return provider_base.trim_end_matches('/').to_string();
    }
    std::env::var(base_env)
        .ok()
        .or_else(|| std::env::var("XAI_BASE_URL").ok())
        .or_else(|| credential_base_url.map(str::to_string))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.x.ai/v1".into())
}

async fn xai_transcribe_audio_part(
    credential: &XaiAudioCredential,
    timeout_seconds: u64,
    bytes: Vec<u8>,
    filename: String,
    mime_type: String,
    language: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = xai_stt_url(&credential.base_url)?;
    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&mime_type)
        .map_err(|error| AppError::BadRequest(format!("invalid audio MIME type: {error}")))?;
    let mut form = reqwest::multipart::Form::new().part("file", file_part);
    if !language.is_empty() {
        form = form.text("language", language.to_string());
    }
    if bool_payload_arg(payload, &["format"], true) {
        form = form.text("format", "true");
    }
    if bool_payload_arg(payload, &["diarize", "diarization"], false) {
        form = form.text("diarize", "true");
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build xAI STT client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .bearer_auth(&credential.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI transcribe_audio failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read xAI transcription response: {error}"
        ))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        return Err(AppError::BadRequest(format!(
            "xAI transcribe_audio returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    extract_transcription_text(&body, &content_type)
}

pub(super) fn xai_stt_url(base_url: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid xAI STT provider URL: {error}")))?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/stt") {
        return Ok(url);
    }
    let path = if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else if path.ends_with("/audio/speech") {
        path.trim_end_matches("/audio/speech").to_string()
    } else if path.ends_with("/audio/transcriptions") {
        path.trim_end_matches("/audio/transcriptions").to_string()
    } else if path.ends_with("/tts") {
        path.trim_end_matches("/tts").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/stt");
    url.set_path(&next);
    Ok(url)
}

pub(super) async fn elevenlabs_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_elevenlabs_audio_credential(store, provider)?;
    let model = payload
        .get("model")
        .or_else(|| payload.get("modelId"))
        .or_else(|| payload.get("model_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "scribe_v2"
            } else {
                configured
            }
        });
    let language_code = payload
        .get("languageCode")
        .or_else(|| payload.get("language_code"))
        .or_else(|| payload.get("language"))
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (bytes, filename, mime_type, source_label) = transcribe_audio_bytes(agent, source).await?;
    ensure_transcribe_audio_size(bytes.len())?;
    let raw_transcript = elevenlabs_transcribe_audio_part(
        &credential,
        provider.timeout_seconds.max(1),
        bytes.clone(),
        filename,
        mime_type.clone(),
        model,
        language_code,
        payload,
    )
    .await?;
    let filtered = is_whisper_hallucination(&raw_transcript);
    let transcript = if filtered {
        String::new()
    } else {
        raw_transcript.clone()
    };
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": "elevenlabs",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "languageCode": language_code,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
    }))?)
}

#[derive(Clone, Debug)]
struct ElevenLabsAudioCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_elevenlabs_audio_credential(
    store: &AppStore,
    provider: &LlmProvider,
) -> AppResult<ElevenLabsAudioCredential> {
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        return Ok(ElevenLabsAudioCredential {
            api_key,
            base_url: elevenlabs_audio_base_url(provider, None),
            source: if provider.api_key.is_some() {
                format!("provider:{}", provider.id)
            } else {
                format!("env:{}", provider.api_key_env)
            },
        });
    }
    if let Some(api_key) = std::env::var("ELEVENLABS_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(ElevenLabsAudioCredential {
            api_key,
            base_url: elevenlabs_audio_base_url(provider, None),
            source: "env:ELEVENLABS_API_KEY".into(),
        });
    }
    let config = store.config()?;
    if let Some(api_key) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| env.get("ELEVENLABS_API_KEY"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let dashboard_base = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| {
                env.get("ELEVENLABS_STT_BASE_URL")
                    .or_else(|| env.get("ELEVENLABS_BASE_URL"))
                    .and_then(Value::as_str)
            });
        return Ok(ElevenLabsAudioCredential {
            api_key,
            base_url: elevenlabs_audio_base_url(provider, dashboard_base),
            source: "dashboardEnv:ELEVENLABS_API_KEY".into(),
        });
    }
    Err(AppError::BadRequest(
        "ELEVENLABS_API_KEY is not set for ElevenLabs STT".into(),
    ))
}

fn elevenlabs_audio_base_url(provider: &LlmProvider, dashboard_base_url: Option<&str>) -> String {
    let provider_base = provider.base_url.trim();
    if !provider_base.is_empty() {
        return provider_base.trim_end_matches('/').to_string();
    }
    std::env::var("ELEVENLABS_STT_BASE_URL")
        .ok()
        .or_else(|| std::env::var("ELEVENLABS_BASE_URL").ok())
        .or_else(|| dashboard_base_url.map(str::to_string))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.elevenlabs.io/v1".into())
}

async fn elevenlabs_transcribe_audio_part(
    credential: &ElevenLabsAudioCredential,
    timeout_seconds: u64,
    bytes: Vec<u8>,
    filename: String,
    mime_type: String,
    model: &str,
    language_code: Option<&str>,
    payload: &Value,
) -> AppResult<String> {
    let url = elevenlabs_speech_to_text_url(&credential.base_url)?;
    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&mime_type)
        .map_err(|error| AppError::BadRequest(format!("invalid audio MIME type: {error}")))?;
    let mut form = reqwest::multipart::Form::new()
        .text("model_id", model.to_string())
        .text(
            "tag_audio_events",
            if bool_payload_arg(payload, &["tagAudioEvents", "tag_audio_events"], false) {
                "true"
            } else {
                "false"
            },
        )
        .text(
            "diarize",
            if bool_payload_arg(payload, &["diarize", "diarization"], false) {
                "true"
            } else {
                "false"
            },
        )
        .part("file", file_part);
    if let Some(language_code) = language_code {
        form = form.text("language_code", language_code.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build ElevenLabs STT client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .header("xi-api-key", &credential.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("ElevenLabs transcribe_audio failed: {error}"))
        })?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read ElevenLabs transcription response: {error}"
        ))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        return Err(AppError::BadRequest(format!(
            "ElevenLabs transcribe_audio returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    extract_transcription_text(&body, &content_type)
}

pub(super) fn elevenlabs_speech_to_text_url(base_url: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid ElevenLabs STT provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/speech-to-text") {
        return Ok(url);
    }
    let path = if path.ends_with("/text-to-speech") {
        path.trim_end_matches("/text-to-speech").to_string()
    } else if path.ends_with("/voices") {
        path.trim_end_matches("/voices").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/speech-to-text");
    url.set_path(&next);
    Ok(url)
}

pub(super) async fn mistral_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let credential = resolve_mistral_audio_credential(store, provider)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let configured = provider.model.trim();
            if configured.is_empty() || configured == "echo" {
                "voxtral-mini-latest"
            } else {
                configured
            }
        });
    let (bytes, filename, mime_type, source_label) = transcribe_audio_bytes(agent, source).await?;
    ensure_transcribe_audio_size(bytes.len())?;
    let raw_transcript = mistral_transcribe_audio_part(
        &credential,
        provider.timeout_seconds.max(1),
        bytes.clone(),
        filename,
        mime_type.clone(),
        model,
        payload,
    )
    .await?;
    let filtered = is_whisper_hallucination(&raw_transcript);
    let transcript = if filtered {
        String::new()
    } else {
        raw_transcript.clone()
    };
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": "mistral",
        "providerId": provider.id,
        "credentialSource": credential.source,
        "model": model,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> }
    }))?)
}

#[derive(Clone, Debug)]
struct MistralAudioCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_mistral_audio_credential(
    store: &AppStore,
    provider: &LlmProvider,
) -> AppResult<MistralAudioCredential> {
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        return Ok(MistralAudioCredential {
            api_key,
            base_url: mistral_audio_base_url(provider, None),
            source: if provider.api_key.is_some() {
                format!("provider:{}", provider.id)
            } else {
                format!("env:{}", provider.api_key_env)
            },
        });
    }
    if let Some(api_key) = std::env::var("MISTRAL_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(MistralAudioCredential {
            api_key,
            base_url: mistral_audio_base_url(provider, None),
            source: "env:MISTRAL_API_KEY".into(),
        });
    }
    let config = store.config()?;
    if let Some(api_key) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| env.get("MISTRAL_API_KEY"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let dashboard_base = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| {
                env.get("MISTRAL_STT_BASE_URL")
                    .or_else(|| env.get("MISTRAL_BASE_URL"))
                    .and_then(Value::as_str)
            });
        return Ok(MistralAudioCredential {
            api_key,
            base_url: mistral_audio_base_url(provider, dashboard_base),
            source: "dashboardEnv:MISTRAL_API_KEY".into(),
        });
    }
    Err(AppError::BadRequest(
        "MISTRAL_API_KEY is not set for Mistral STT".into(),
    ))
}

fn mistral_audio_base_url(provider: &LlmProvider, dashboard_base_url: Option<&str>) -> String {
    let provider_base = provider.base_url.trim();
    if !provider_base.is_empty() {
        return provider_base.trim_end_matches('/').to_string();
    }
    std::env::var("MISTRAL_STT_BASE_URL")
        .ok()
        .or_else(|| std::env::var("MISTRAL_BASE_URL").ok())
        .or_else(|| dashboard_base_url.map(str::to_string))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.mistral.ai/v1".into())
}

async fn mistral_transcribe_audio_part(
    credential: &MistralAudioCredential,
    timeout_seconds: u64,
    bytes: Vec<u8>,
    filename: String,
    mime_type: String,
    model: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = mistral_audio_transcriptions_url(&credential.base_url)?;
    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&mime_type)
        .map_err(|error| AppError::BadRequest(format!("invalid audio MIME type: {error}")))?;
    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .part("file", file_part);
    if let Some(language) = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form = form.text("language", language.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Mistral STT client: {error}"))
        })?;
    let response = client
        .post(url.clone())
        .bearer_auth(&credential.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Mistral transcribe_audio failed: {error}"))
        })?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read Mistral transcription response: {error}"
        ))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        return Err(AppError::BadRequest(format!(
            "Mistral transcribe_audio returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    extract_transcription_text(&body, &content_type)
}

pub(super) fn mistral_audio_transcriptions_url(base_url: &str) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid Mistral STT provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/audio/transcriptions") {
        return Ok(url);
    }
    let path = if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else if path.ends_with("/audio/speech") {
        path.trim_end_matches("/audio/speech").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/audio/transcriptions");
    url.set_path(&next);
    Ok(url)
}

fn bool_payload_arg(payload: &Value, keys: &[&str], default: bool) -> bool {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(flag) = value.as_bool() {
                return flag;
            }
            if let Some(text) = value.as_str() {
                let normalized = text.trim().to_ascii_lowercase();
                if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
                    return true;
                }
                if matches!(normalized.as_str(), "0" | "false" | "no" | "off") {
                    return false;
                }
            }
        }
    }
    default
}

fn local_transcription_path(agent: &AgentDefinition, source: &str) -> AppResult<PathBuf> {
    let source = source.trim();
    if source.starts_with("data:audio/")
        || source.starts_with("http://")
        || source.starts_with("https://")
    {
        return Err(AppError::BadRequest(
            "local_command STT requires a local workspace audio path".into(),
        ));
    }
    let local_source = if source.starts_with("file://") {
        reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")))
    } else {
        PathBuf::from(source)
    };
    let root = workspace_root(agent)?;
    let path_text = local_source.to_string_lossy();
    let path = resolve_workspace_path(&root, &path_text)?;
    if !path.is_file() {
        return Err(AppError::BadRequest(format!(
            "local_command STT path is not a file: {}",
            path.display()
        )));
    }
    Ok(path)
}

fn render_local_stt_command(
    template: &str,
    input_path: &Path,
    output_path: &Path,
    model: &str,
    language: &str,
) -> String {
    let input = shell_quote_path(input_path);
    let output = shell_quote_path(output_path);
    template
        .replace("{input_path}", &input)
        .replace("{path}", &input)
        .replace("{output_path}", &output)
        .replace(
            "{output_dir}",
            &shell_quote_path(output_path.parent().unwrap_or_else(|| Path::new("."))),
        )
        .replace("{model}", &shell_quote_value(model))
        .replace("{language}", &shell_quote_value(language))
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote_value(&path.to_string_lossy())
}

fn shell_quote_value(value: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn default_desktop_python_command() -> String {
    if command_available("python") {
        return "python".into();
    }
    if command_available("py") {
        return "py".into();
    }
    [
        r"F:\python313\python.exe",
        r"F:\python312\python.exe",
        r"F:\python311\python.exe",
        r"C:\Python313\python.exe",
        r"C:\Python312\python.exe",
        r"C:\Python311\python.exe",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .unwrap_or("python")
    .to_string()
}

fn read_command_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let _ = pipe.read_to_end(&mut output);
        output
    })
}

#[cfg(target_os = "windows")]
fn escape_windows_batch_script(command_text: &str) -> String {
    command_text.replace('%', "%%")
}

fn run_shell_command_with_timeout(command_text: &str, timeout_seconds: u64) -> AppResult<String> {
    #[cfg(target_os = "windows")]
    let script_path = {
        let path = std::env::temp_dir().join(format!(
            "synthchat-local-audio-{}.cmd",
            timestamp_millis()?
        ));
        let command_script = escape_windows_batch_script(command_text);
        fs::write(&path, format!("@echo off\r\n{command_script}\r\n"))?;
        Some(path)
    };
    #[cfg(not(target_os = "windows"))]
    let script_path: Option<PathBuf> = None;
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        let script = script_path.as_ref().ok_or_else(|| {
            AppError::BadRequest("failed to create local audio command script".into())
        })?;
        command.arg("/C").arg(script);
        command
    };
    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut command = Command::new("sh");
        command.arg("-c").arg(command_text);
        command
    };
    command.hide_window();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Some(path) = &script_path {
                let _ = fs::remove_file(path);
            }
            return Err(AppError::BadRequest(format!(
                "failed to start local audio command: {error}"
            )));
        }
    };
    let stdout = child.stdout.take().map(read_command_pipe);
    let stderr = child.stderr.take().map(read_command_pipe);
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let status = child.wait()?;
            let stdout = stdout
                .map(|handle| handle.join().unwrap_or_default())
                .unwrap_or_default();
            let stderr = stderr
                .map(|handle| handle.join().unwrap_or_default())
                .unwrap_or_default();
            if let Some(path) = &script_path {
                let _ = fs::remove_file(path);
            }
            if !status.success() {
                let stderr = String::from_utf8_lossy(&stderr);
                let stdout = String::from_utf8_lossy(&stdout);
                let output = [stderr.trim(), stdout.trim()]
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(AppError::BadRequest(format!(
                    "local audio command exited with {status}: {} [command: {}]",
                    truncate_output(&output, 2000),
                    truncate_output(command_text, 1200)
                )));
            }
            return Ok(String::from_utf8_lossy(&stdout).trim().to_string());
        }
        if started.elapsed() >= Duration::from_secs(timeout_seconds) {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = stdout
                .map(|handle| handle.join().unwrap_or_default())
                .unwrap_or_default();
            let stderr = stderr
                .map(|handle| handle.join().unwrap_or_default())
                .unwrap_or_default();
            if let Some(path) = &script_path {
                let _ = fs::remove_file(path);
            }
            let stderr = String::from_utf8_lossy(&stderr);
            let stdout = String::from_utf8_lossy(&stdout);
            return Err(AppError::BadRequest(format!(
                "local audio command timed out after {timeout_seconds}s; stderr: {}; stdout: {}",
                truncate_output(&stderr, 1000),
                truncate_output(&stdout, 1000)
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn read_local_stt_output(output_path: &Path, stdout: &str) -> AppResult<String> {
    if let Ok(content) = fs::read_to_string(output_path) {
        let content = content.trim().to_string();
        if !content.is_empty() {
            return Ok(content);
        }
    }
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return Ok(stdout.to_string());
    }
    Err(AppError::BadRequest(format!(
        "local STT command produced no transcript at {} and no stdout",
        output_path.display()
    )))
}

async fn openai_compatible_transcribe_audio_part(
    provider: &LlmProvider,
    bytes: Vec<u8>,
    filename: String,
    mime_type: String,
    model: &str,
    payload: &Value,
) -> AppResult<String> {
    ensure_transcribe_audio_size(bytes.len())?;
    let url = audio_transcriptions_url(provider)?;
    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename.clone())
        .mime_str(&mime_type)
        .map_err(|error| AppError::BadRequest(format!("invalid audio MIME type: {error}")))?;
    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .part("file", file_part);
    if let Some(language) = payload
        .get("language")
        .or_else(|| payload.get("lang"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form = form.text("language", language.to_string());
    }
    if let Some(prompt) = payload
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form = form.text("prompt", prompt.to_string());
    }
    if let Some(format) = payload
        .get("responseFormat")
        .or_else(|| payload.get("response_format"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form = form.text("response_format", format.to_string());
    }
    if let Some(temperature) = payload.get("temperature").and_then(Value::as_f64) {
        form = form.text("temperature", temperature.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build transcription client: {error}"))
        })?;
    let mut request = client.post(url.clone()).multipart(form);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("transcribe_audio failed: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read transcription response: {error}"))
    })?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        return Err(AppError::BadRequest(format!(
            "transcribe_audio returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    extract_transcription_text(&body, &content_type)
}

async fn openai_compatible_transcribe_wav_chunks(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    path: &Path,
    model: &str,
    payload: &Value,
) -> AppResult<String> {
    let source_label = path.to_string_lossy().to_string();
    let original_size = fs::metadata(path)?.len();
    let chunk_paths = split_wav_for_transcription(path, MAX_TRANSCRIBE_AUDIO_BYTES)?;
    if chunk_paths.is_empty() {
        return Err(AppError::BadRequest(
            "chunked transcription did not create any audio chunks".into(),
        ));
    }
    let mut transcripts = Vec::new();
    let mut raw_transcripts = Vec::new();
    let mut filtered_chunks = 0usize;
    let mut chunk_results = Vec::new();
    let chunk_count = chunk_paths.len();
    let result = async {
        for (index, chunk_path) in chunk_paths.iter().enumerate() {
            let bytes = fs::read(chunk_path)?;
            let filename = chunk_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("audio-chunk.wav")
                .to_string();
            let raw = openai_compatible_transcribe_audio_part(
                provider,
                bytes,
                filename,
                "audio/wav".into(),
                model,
                payload,
            )
            .await
            .map_err(|error| {
                AppError::BadRequest(format!(
                    "chunk {}/{} transcription failed: {error}",
                    index + 1,
                    chunk_count
                ))
            })?;
            let filtered = is_whisper_hallucination(&raw);
            raw_transcripts.push(raw.clone());
            if filtered {
                filtered_chunks += 1;
            } else {
                let text = raw.trim();
                if !text.is_empty() {
                    transcripts.push(text.to_string());
                }
            }
            chunk_results.push(json!({
                "index": index + 1,
                "filtered": filtered,
                "transcriptChars": if filtered { 0 } else { raw.trim().chars().count() }
            }));
        }
        Ok::<_, AppError>(())
    }
    .await;
    for chunk_path in &chunk_paths {
        let _ = fs::remove_file(chunk_path);
    }
    result?;
    let transcript = transcripts.join(" ").trim().to_string();
    let raw_transcript = raw_transcripts.join(" ").trim().to_string();
    let filtered = filtered_chunks == chunk_count && transcript.is_empty();
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": provider.provider_type,
        "providerId": provider.id,
        "model": model,
        "source": source_label,
        "mimeType": "audio/wav",
        "sizeBytes": original_size,
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript,
        "filtered": filtered,
        "filteredReason": if filtered { Some("whisper_silence_hallucination") } else { None::<&str> },
        "rawTranscript": if filtered { Some(raw_transcript) } else { None::<String> },
        "chunked": true,
        "chunks": chunk_count,
        "filteredChunks": filtered_chunks,
        "chunkResults": chunk_results
    }))?)
}

pub(super) fn audio_transcriptions_url(provider: &LlmProvider) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim()).map_err(|error| {
        AppError::BadRequest(format!("invalid transcription provider URL: {error}"))
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/audio/transcriptions") {
        return Ok(url);
    }
    let path = if path.ends_with("/audio/speech") {
        path.trim_end_matches("/audio/speech").to_string()
    } else if path.ends_with("/chat/completions") {
        path.trim_end_matches("/chat/completions").to_string()
    } else if path.ends_with("/responses") {
        path.trim_end_matches("/responses").to_string()
    } else {
        path
    };
    let mut next = path.trim_end_matches('/').to_string();
    next.push_str("/audio/transcriptions");
    url.set_path(&next);
    Ok(url)
}

fn oversized_local_wav_for_transcription(
    agent: &AgentDefinition,
    source: &str,
) -> AppResult<Option<PathBuf>> {
    let source = source.trim();
    if source.starts_with("data:audio/")
        || source.starts_with("http://")
        || source.starts_with("https://")
    {
        return Ok(None);
    }
    let local_source = if source.starts_with("file://") {
        reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")))
    } else {
        PathBuf::from(source)
    };
    let root = workspace_root(agent)?;
    let path_text = local_source.to_string_lossy();
    let path = resolve_workspace_path(&root, &path_text)?;
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
    {
        return Ok(None);
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() as usize > MAX_TRANSCRIBE_AUDIO_BYTES {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

pub(super) async fn transcribe_audio_bytes(
    agent: &AgentDefinition,
    source: &str,
) -> AppResult<(Vec<u8>, String, String, String)> {
    let source = source.trim();
    if source.starts_with("data:audio/") {
        let (mime, bytes) = decode_audio_data_url(source)?;
        ensure_transcribe_audio_size(bytes.len())?;
        let filename = format!("audio.{}", audio_extension_from_mime(&mime));
        return Ok((bytes, filename, mime, "inline data audio".into()));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        validate_web_url(source)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("SynthChat-agent/1.0")
            .build()
            .map_err(|error| {
                AppError::BadRequest(format!("failed to build audio downloader: {error}"))
            })?;
        let response = client
            .get(source)
            .send()
            .await
            .map_err(|error| AppError::BadRequest(format!("audio download failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::BadRequest(format!(
                "audio download returned HTTP {}",
                status.as_u16()
            )));
        }
        if let Some(length) = response.content_length() {
            ensure_transcribe_audio_size(length as usize)?;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::BadRequest(format!("failed to read audio bytes: {error}")))?
            .to_vec();
        ensure_transcribe_audio_size(bytes.len())?;
        let mime = audio_mime_from_source(source, Some(&content_type));
        let filename = remote_audio_filename(source, &mime);
        return Ok((bytes, filename, mime, source.to_string()));
    }
    let local_source = if source.starts_with("file://") {
        reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")))
    } else {
        PathBuf::from(source)
    };
    let root = workspace_root(agent)?;
    let path_text = local_source.to_string_lossy();
    let path = resolve_workspace_path(&root, &path_text)?;
    let metadata = fs::metadata(&path)?;
    ensure_transcribe_audio_size(metadata.len() as usize)?;
    let bytes = fs::read(&path)?;
    let mime = audio_mime_from_path(&path);
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio")
        .to_string();
    Ok((bytes, filename, mime, path.to_string_lossy().to_string()))
}

pub(super) fn split_wav_for_transcription(
    path: &Path,
    max_file_size: usize,
) -> AppResult<Vec<PathBuf>> {
    let bytes = fs::read(path)?;
    if bytes.len() <= max_file_size {
        return Ok(vec![path.to_path_buf()]);
    }
    let layout = parse_wav_layout(&bytes)?;
    let header_len = layout.data_start;
    if max_file_size <= header_len {
        return Err(AppError::BadRequest(
            "STT max file size is too small for WAV chunking".into(),
        ));
    }
    let max_data_bytes = ((max_file_size - header_len) / layout.block_align) * layout.block_align;
    if max_data_bytes == 0 {
        return Err(AppError::BadRequest(
            "STT max file size leaves no aligned WAV audio frames".into(),
        ));
    }
    let data_end = layout
        .data_start
        .checked_add(layout.data_size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| AppError::BadRequest("invalid WAV data chunk size".into()))?;
    let data = &bytes[layout.data_start..data_end];
    let mut chunks = Vec::new();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("recording");
    for (index, data_chunk) in data.chunks(max_data_bytes).enumerate() {
        if data_chunk.is_empty() {
            continue;
        }
        let mut chunk = bytes[..layout.data_start].to_vec();
        let chunk_riff_size = (chunk.len() + data_chunk.len() - 8) as u32;
        patch_wav_u32(&mut chunk, 4, chunk_riff_size)?;
        patch_wav_u32(&mut chunk, layout.data_size_offset, data_chunk.len() as u32)?;
        chunk.extend_from_slice(data_chunk);
        let chunk_path = std::env::temp_dir().join(format!(
            "{stem}_chunk{:03}_{}.wav",
            index + 1,
            timestamp_millis()?
        ));
        fs::write(&chunk_path, chunk)?;
        chunks.push(chunk_path);
    }
    Ok(chunks)
}

struct WavLayout {
    data_start: usize,
    data_size: usize,
    data_size_offset: usize,
    block_align: usize,
}

fn parse_wav_layout(bytes: &[u8]) -> AppResult<WavLayout> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AppError::BadRequest(
            "chunked transcription only supports RIFF/WAVE files".into(),
        ));
    }
    let mut cursor = 12usize;
    let mut data_start = None;
    let mut data_size = None;
    let mut data_size_offset = None;
    let mut block_align = None;
    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = read_wav_u32(bytes, cursor + 4)? as usize;
        let chunk_data_start = cursor + 8;
        let chunk_data_end = chunk_data_start
            .checked_add(chunk_size)
            .ok_or_else(|| AppError::BadRequest("invalid WAV chunk size".into()))?;
        if chunk_data_end > bytes.len() {
            return Err(AppError::BadRequest("invalid WAV chunk boundary".into()));
        }
        if chunk_id == b"fmt " && chunk_size >= 14 {
            block_align = Some(read_wav_u16(bytes, chunk_data_start + 12)? as usize);
        } else if chunk_id == b"data" {
            data_start = Some(chunk_data_start);
            data_size = Some(chunk_size);
            data_size_offset = Some(cursor + 4);
            break;
        }
        cursor = chunk_data_end + (chunk_size % 2);
    }
    Ok(WavLayout {
        data_start: data_start
            .ok_or_else(|| AppError::BadRequest("WAV data chunk missing".into()))?,
        data_size: data_size.unwrap_or(0),
        data_size_offset: data_size_offset.unwrap_or(0),
        block_align: block_align.unwrap_or(1).max(1),
    })
}

fn read_wav_u16(bytes: &[u8], offset: usize) -> AppResult<u16> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| AppError::BadRequest("invalid WAV u16 offset".into()))?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn read_wav_u32(bytes: &[u8], offset: usize) -> AppResult<u32> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| AppError::BadRequest("invalid WAV u32 offset".into()))?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

fn patch_wav_u32(bytes: &mut [u8], offset: usize, value: u32) -> AppResult<()> {
    let data = bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| AppError::BadRequest("invalid WAV patch offset".into()))?;
    data.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub(super) fn ensure_transcribe_audio_size(size: usize) -> AppResult<()> {
    if size > MAX_TRANSCRIBE_AUDIO_BYTES {
        Err(AppError::BadRequest(format!(
            "audio is too large: {} bytes exceeds {} bytes",
            size, MAX_TRANSCRIBE_AUDIO_BYTES
        )))
    } else {
        Ok(())
    }
}

pub(super) fn decode_audio_data_url(source: &str) -> AppResult<(String, Vec<u8>)> {
    use base64::Engine;
    let (meta, data) = source
        .split_once(',')
        .ok_or_else(|| AppError::BadRequest("invalid audio data URL".into()))?;
    if !meta.contains(";base64") {
        return Err(AppError::BadRequest(
            "audio data URL must use base64 encoding".into(),
        ));
    }
    let mime = meta
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or("audio/mpeg")
        .to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| AppError::BadRequest(format!("invalid audio data base64: {error}")))?;
    Ok((mime, bytes))
}

pub(super) fn audio_mime_from_path(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    audio_mime_from_extension(&ext).to_string()
}

pub(super) fn audio_mime_from_source(source: &str, content_type: Option<&str>) -> String {
    if let Some(content_type) = content_type
        .map(str::trim)
        .filter(|value| value.to_ascii_lowercase().starts_with("audio/"))
    {
        return content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .to_string();
    }
    let ext = reqwest::Url::parse(source)
        .ok()
        .and_then(|url| {
            Path::new(url.path())
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .or_else(|| {
            Path::new(source)
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    audio_mime_from_extension(&ext).to_string()
}

pub(super) fn audio_mime_from_extension(ext: &str) -> &'static str {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "silk" => "audio/silk",
        "amr" => "audio/amr",
        "m4a" | "mp4" => "audio/mp4",
        "mpeg" | "mpga" => "audio/mpeg",
        "webm" => "audio/webm",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

pub(super) fn audio_extension_from_mime(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or(mime).trim() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp4" | "audio/m4a" => "m4a",
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/silk" => "silk",
        "audio/amr" => "amr",
        _ => "bin",
    }
}

pub(super) fn is_whisper_hallucination(transcript: &str) -> bool {
    let cleaned = transcript.trim().to_lowercase();
    if cleaned.is_empty() {
        return true;
    }
    let exact = cleaned.trim_matches(|ch| matches!(ch, '.' | '!' | '?' | ',' | ' '));
    const HALLUCINATIONS: &[&str] = &[
        "thank you.",
        "thank you",
        "thanks for watching.",
        "thanks for watching",
        "subscribe to my channel.",
        "subscribe to my channel",
        "like and subscribe.",
        "like and subscribe",
        "please subscribe.",
        "please subscribe",
        "thank you for watching.",
        "thank you for watching",
        "bye.",
        "bye",
        "you",
        "the end.",
        "the end",
        "продолжение следует",
        "продолжение следует...",
        "sous-titres",
        "sous-titres réalisés par la communauté d'amara.org",
        "sottotitoli creati dalla comunità amara.org",
        "untertitel von stephanie geiges",
        "amara.org",
        "www.mooji.org",
        "ご視聴ありがとうございました",
    ];
    if HALLUCINATIONS.iter().any(|phrase| {
        exact
            == phrase
                .trim_matches(|ch| matches!(ch, '.' | '!' | '?' | ',' | ' '))
                .to_lowercase()
    }) {
        return true;
    }
    let tokens = cleaned
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '.' | ',' | '!' | '?'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    !tokens.is_empty()
        && tokens.iter().all(|token| {
            matches!(
                *token,
                "thank" | "you" | "thanks" | "bye" | "ok" | "okay" | "the" | "end"
            )
        })
}

pub(super) fn remote_audio_filename(source: &str, mime: &str) -> String {
    reqwest::Url::parse(source)
        .ok()
        .and_then(|url| {
            Path::new(url.path())
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("audio.{}", audio_extension_from_mime(mime)))
}

pub(super) fn extract_transcription_text(bytes: &[u8], content_type: &str) -> AppResult<String> {
    let body_text = String::from_utf8_lossy(bytes).trim().to_string();
    if content_type.contains("json") || body_text.starts_with('{') {
        let value = serde_json::from_slice::<Value>(bytes).map_err(|error| {
            AppError::BadRequest(format!("invalid transcription JSON: {error}"))
        })?;
        let text = value
            .get("text")
            .or_else(|| value.get("transcript"))
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::BadRequest("transcription response missing text".into()))?;
        return Ok(text.to_string());
    }
    if body_text.is_empty() {
        Err(AppError::BadRequest(
            "transcription response was empty".into(),
        ))
    } else {
        Ok(body_text)
    }
}

pub(super) fn provider_api_key(inline: &Option<String>, env_name: &str) -> Option<String> {
    inline
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let env_name = env_name.trim();
            if env_name.is_empty() {
                None
            } else {
                std::env::var(env_name).ok()
            }
        })
}

pub(super) fn string_arg(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn required_string_arg(
    payload: &Value,
    keys: &[&str],
    tool_name: &str,
) -> AppResult<String> {
    string_arg(payload, keys).ok_or_else(|| {
        AppError::BadRequest(format!(
            "{tool_name} requires payload.{}",
            keys.first().copied().unwrap_or("value")
        ))
    })
}

pub(super) fn decode_base64_image(value: &str) -> AppResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| AppError::BadRequest(format!("invalid image base64: {error}")))
}

pub(super) async fn download_image_bytes(
    client: &reqwest::Client,
    image_url: &str,
) -> AppResult<(Vec<u8>, String)> {
    let response = client
        .get(image_url)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("image download failed: {error}")))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read image bytes: {error}")))?
        .to_vec();
    Ok((bytes, image_extension_from_content_type(&content_type)))
}

pub(super) fn image_extension_from_content_type(content_type: &str) -> String {
    if content_type.contains("jpeg") || content_type.contains("jpg") {
        "jpg".into()
    } else if content_type.contains("webp") {
        "webp".into()
    } else if content_type.contains("gif") {
        "gif".into()
    } else {
        "png".into()
    }
}

pub(super) async fn vision_analyze_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let prompt = string_arg(payload, &["prompt", "question"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Analyze this image.".into());
    let source = string_arg(
        payload,
        &["path", "imagePath", "image_url", "imageUrl", "url"],
    )
    .filter(|value| !value.trim().is_empty())
    .ok_or_else(|| {
        AppError::BadRequest(
            "vision_analyze requires payload.path, payload.image_url, or payload.url".into(),
        )
    })?;
    let provider = resolve_vision_provider(store)?
        .ok_or_else(|| AppError::BadRequest("no enabled vision provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "openai_compatible" | "compatible" | "custom" | "" => {
            openai_compatible_vision_analyze(
                store, agent, run_id, &provider, &prompt, &source, payload,
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported vision provider type: {other}"
        ))),
    }
}

pub(super) async fn openai_compatible_vision_analyze(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &VisionProvider,
    prompt: &str,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let (image_url, source_label) = vision_image_url(agent, source)?;
    let url = vision_chat_completions_url(provider)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&provider.model);
    let max_tokens = payload
        .get("maxTokens")
        .or_else(|| payload.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(800)
        .clamp(64, 8192);
    let body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": image_url}}
            ]
        }],
        "max_tokens": max_tokens
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build vision client: {error}")))?;
    let mut request = client.post(url.clone()).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("vision_analyze failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read vision response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "vision_analyze returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid vision JSON: {error}")))?;
    let analysis = extract_vision_message_content(&value).ok_or_else(|| {
        AppError::BadRequest(format!(
            "vision response missing choices[0].message.content: {}",
            truncate_output(&text, 2000)
        ))
    })?;
    let artifact_path = store.save_tool_artifact(run_id, "vision_analyze", &analysis)?;
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "prompt": prompt,
        "source": source_label,
        "artifactPath": artifact_path.to_string_lossy(),
        "analysis": analysis
    }))?)
}

pub(super) fn vision_chat_completions_url(provider: &VisionProvider) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid vision provider URL: {error}")))?;
    if !url.path().ends_with("/chat/completions") {
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push_str("/chat/completions");
        url.set_path(&path);
    }
    Ok(url)
}

pub(super) fn vision_image_url(
    agent: &AgentDefinition,
    source: &str,
) -> AppResult<(String, String)> {
    let source = source.trim();
    if source.starts_with("data:image/") {
        return Ok((source.to_string(), "inline data image".into()));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        validate_web_url(source)?;
        return Ok((source.to_string(), source.to_string()));
    }
    let local_source = if source.starts_with("file://") {
        reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")))
    } else {
        PathBuf::from(source)
    };
    let root = workspace_root(agent)?;
    let path_text = local_source.to_string_lossy();
    let path = resolve_workspace_path(&root, &path_text)?;
    let bytes = fs::read(&path)?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mime = image_mime_from_path(&path);
    Ok((
        format!("data:{mime};base64,{encoded}"),
        path.to_string_lossy().to_string(),
    ))
}

pub(super) fn image_mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

pub(super) fn extract_vision_message_content(value: &Value) -> Option<String> {
    let content = value.pointer("/choices/0/message/content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .or_else(|| part.get("content").and_then(Value::as_str))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

const MAX_VIDEO_ANALYZE_BYTES: usize = 50 * 1024 * 1024;

pub(super) async fn video_analyze_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let question = string_arg(payload, &["question", "prompt"])
        .unwrap_or_else(|| "Fully describe and explain everything happening in this video.".into());
    let source = string_arg(
        payload,
        &[
            "videoUrl",
            "video_url",
            "url",
            "path",
            "videoPath",
            "video_path",
        ],
    )
    .ok_or_else(|| {
        AppError::BadRequest(
            "video_analyze requires payload.videoUrl, payload.url, or payload.path".into(),
        )
    })?;
    let provider = resolve_vision_provider(store)?
        .ok_or_else(|| AppError::BadRequest("no enabled vision provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "openai_compatible" | "compatible" | "custom" | "" => {
            openai_compatible_video_analyze(
                store, agent, run_id, &provider, &question, &source, payload,
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported video analysis provider type: {other}"
        ))),
    }
}

pub(super) async fn openai_compatible_video_analyze(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &VisionProvider,
    question: &str,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let (video_url, source_label, size_bytes, mime_type) =
        video_data_url(agent, source, payload).await?;
    let url = vision_chat_completions_url(provider)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&provider.model);
    let max_tokens = payload
        .get("maxTokens")
        .or_else(|| payload.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(4000)
        .clamp(256, 8192);
    let prompt = format!(
        "Fully describe and explain everything happening in this video, including visual content, motion, text overlays, scene transitions, and any visible context. Then answer the question:\n\n{question}"
    );
    let body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "video_url", "video_url": {"url": video_url}}
            ]
        }],
        "max_tokens": max_tokens
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(180)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build video client: {error}")))?;
    let mut request = client.post(url.clone()).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("video_analyze failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read video analysis response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "video_analyze returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid video analysis JSON: {error}")))?;
    let analysis = extract_vision_message_content(&value).ok_or_else(|| {
        AppError::BadRequest(format!(
            "video analysis response missing choices[0].message.content: {}",
            truncate_output(&text, 2000)
        ))
    })?;
    let artifact_path = store.save_tool_artifact(run_id, "video_analyze", &analysis)?;
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "question": question,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": size_bytes,
        "artifactPath": artifact_path.to_string_lossy(),
        "analysis": analysis
    }))?)
}

pub(super) fn resolve_vision_provider(store: &AppStore) -> AppResult<Option<VisionProvider>> {
    let assignment = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == "vision");
    let Some(assignment) = assignment else {
        return store.enabled_vision_provider();
    };

    let provider_choice = assignment.provider.trim();
    let model_choice = assignment.model.trim();
    let base_url_choice = assignment.base_url.trim();
    let api_key_choice = assignment.api_key.trim();
    let timeout = assignment.timeout.max(1);

    if !base_url_choice.is_empty() {
        let fallback_model = if model_choice.is_empty() {
            store
                .enabled_vision_provider()
                .ok()
                .flatten()
                .map(|provider| provider.model)
                .unwrap_or_default()
        } else {
            model_choice.to_string()
        };
        if fallback_model.trim().is_empty() {
            return Err(AppError::BadRequest(
                "auxiliary vision custom provider requires a model".into(),
            ));
        }
        return Ok(Some(VisionProvider {
            id: "auxiliary-vision-custom".into(),
            name: "Vision auxiliary".into(),
            provider_type: "openai-compatible".into(),
            base_url: base_url_choice.into(),
            api_key_env: String::new(),
            api_key: (!api_key_choice.is_empty()).then(|| api_key_choice.to_string()),
            model: fallback_model,
            enabled: true,
            timeout_seconds: timeout,
        }));
    }

    let mut provider = if provider_choice.is_empty() || provider_choice.eq_ignore_ascii_case("auto")
    {
        store.enabled_vision_provider()?
    } else {
        resolve_named_vision_provider(store, provider_choice)?
    };

    if let Some(provider) = provider.as_mut() {
        if !model_choice.is_empty() {
            provider.model = model_choice.to_string();
        }
        provider.timeout_seconds = timeout;
    }
    Ok(provider)
}

fn resolve_named_vision_provider(
    store: &AppStore,
    provider_choice: &str,
) -> AppResult<Option<VisionProvider>> {
    let normalized = normalize_provider_choice(provider_choice);
    let providers = store.vision_providers()?;
    Ok(providers.into_iter().find(|provider| {
        provider.enabled
            && !provider.base_url.trim().is_empty()
            && !provider.model.trim().is_empty()
            && [
                provider.id.as_str(),
                provider.name.as_str(),
                provider.provider_type.as_str(),
            ]
            .iter()
            .any(|candidate| normalize_provider_choice(candidate) == normalized)
    }))
}

fn normalize_provider_choice(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

pub(super) async fn video_data_url(
    agent: &AgentDefinition,
    source: &str,
    payload: &Value,
) -> AppResult<(String, String, usize, String)> {
    let source = source.trim();
    if source.starts_with("data:video/") {
        let mime = source
            .split(';')
            .next()
            .unwrap_or("data:video/mp4")
            .trim_start_matches("data:")
            .to_string();
        return Ok((source.to_string(), "inline data video".into(), 0, mime));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        validate_web_url(source)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(
                payload
                    .get("downloadTimeoutSeconds")
                    .or_else(|| payload.get("download_timeout_seconds"))
                    .and_then(Value::as_u64)
                    .unwrap_or(60)
                    .clamp(5, 180),
            ))
            .user_agent("SynthChat-agent/1.0")
            .build()
            .map_err(|error| {
                AppError::BadRequest(format!("failed to build video downloader: {error}"))
            })?;
        let response = client
            .get(source)
            .send()
            .await
            .map_err(|error| AppError::BadRequest(format!("video download failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::BadRequest(format!(
                "video download returned HTTP {}",
                status.as_u16()
            )));
        }
        if let Some(length) = response.content_length() {
            if length as usize > MAX_VIDEO_ANALYZE_BYTES {
                return Err(AppError::BadRequest(format!(
                    "video is too large: {} bytes exceeds {} bytes",
                    length, MAX_VIDEO_ANALYZE_BYTES
                )));
            }
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.bytes().await.map_err(|error| {
            AppError::BadRequest(format!("failed to read video bytes: {error}"))
        })?;
        if bytes.len() > MAX_VIDEO_ANALYZE_BYTES {
            return Err(AppError::BadRequest(format!(
                "video is too large: {} bytes exceeds {} bytes",
                bytes.len(),
                MAX_VIDEO_ANALYZE_BYTES
            )));
        }
        let mime = video_mime_from_source(source, Some(&content_type))
            .ok_or_else(|| AppError::BadRequest("unsupported video content type".into()))?;
        return Ok((
            encode_video_data_url(&bytes, &mime),
            source.to_string(),
            bytes.len(),
            mime,
        ));
    }
    let local_source = if source.starts_with("file://") {
        reqwest::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(source.trim_start_matches("file://")))
    } else {
        PathBuf::from(source)
    };
    let root = workspace_root(agent)?;
    let path_text = local_source.to_string_lossy();
    let path = resolve_workspace_path(&root, &path_text)?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() as usize > MAX_VIDEO_ANALYZE_BYTES {
        return Err(AppError::BadRequest(format!(
            "video is too large: {} bytes exceeds {} bytes",
            metadata.len(),
            MAX_VIDEO_ANALYZE_BYTES
        )));
    }
    let mime = video_mime_from_path(&path).ok_or_else(|| {
        AppError::BadRequest(format!("unsupported video format: {}", path.display()))
    })?;
    let bytes = fs::read(&path)?;
    Ok((
        encode_video_data_url(&bytes, &mime),
        path.to_string_lossy().to_string(),
        bytes.len(),
        mime,
    ))
}

pub(super) fn encode_video_data_url(bytes: &[u8], mime: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{encoded}")
}

pub(super) fn video_mime_from_path(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    video_mime_from_extension(&ext)
}

pub(super) fn video_mime_from_source(source: &str, content_type: Option<&str>) -> Option<String> {
    if let Some(content_type) = content_type
        .map(str::trim)
        .filter(|value| value.to_ascii_lowercase().starts_with("video/"))
    {
        return Some(
            content_type
                .split(';')
                .next()
                .unwrap_or(content_type)
                .to_string(),
        );
    }
    let ext = reqwest::Url::parse(source)
        .ok()
        .and_then(|url| {
            Path::new(url.path())
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_string)
        })
        .or_else(|| {
            Path::new(source)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    video_mime_from_extension(&ext)
}

pub(super) fn video_mime_from_extension(ext: &str) -> Option<String> {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "mp4" => Some("video/mp4".into()),
        "webm" => Some("video/webm".into()),
        "mov" => Some("video/quicktime".into()),
        "avi" => Some("video/mp4".into()),
        "mkv" => Some("video/mp4".into()),
        "mpeg" | "mpg" => Some("video/mpeg".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_provider_kind_accepts_settings_openai_type() {
        let provider = ImageProvider {
            id: "img".into(),
            name: "OpenAI Image".into(),
            provider_type: "openai_image".into(),
            base_url: "https://api.apimart.ai/v1".into(),
            api_key_env: String::new(),
            api_key: None,
            model: "gpt-image-2".into(),
            enabled: true,
            timeout_seconds: 300,
            use_system_proxy: true,
        };

        assert_eq!(
            image_provider_kind(&provider),
            ImageProviderKind::OpenAiCompatible
        );
    }

    #[test]
    fn gpt_image_2_request_body_matches_apimart_shape() {
        let body = gpt_image_2_request_body(
            "draw",
            &json!({
                "n": 4,
                "size": "1024x1024",
                "resolution": "2k",
                "image_urls": ["https://example.com/ref.png"],
                "extra": {"official_fallback": true}
            }),
        );

        assert_eq!(body["model"], "gpt-image-2");
        assert_eq!(body["prompt"], "draw");
        assert_eq!(body["n"], 1);
        assert_eq!(body["size"], "1:1");
        assert_eq!(body["resolution"], "2k");
        assert_eq!(body["image_urls"][0], "https://example.com/ref.png");
        assert_eq!(body["official_fallback"], true);
    }

    #[test]
    fn gpt_image_2_task_id_reads_apimart_submit_response() {
        let value = json!({
            "code": 200,
            "data": [
                {
                    "status": "submitted",
                    "task_id": "task_01"
                }
            ]
        });

        assert_eq!(gpt_image_2_task_id(&value).as_deref(), Some("task_01"));
    }

    #[test]
    fn gpt_image_2_result_urls_reads_apimart_task_response() {
        let value = json!({
            "code": 200,
            "data": {
                "status": "completed",
                "result": {
                    "images": [
                        {
                            "url": [
                                "https://upload.apimart.ai/f/image/out.png"
                            ]
                        }
                    ]
                }
            }
        });

        assert_eq!(
            gpt_image_2_result_urls(&value),
            vec!["https://upload.apimart.ai/f/image/out.png".to_string()]
        );
    }

}
