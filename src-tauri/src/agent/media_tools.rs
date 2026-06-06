use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{AgentDefinition, ImageProvider, LlmProvider, VideoProvider, VisionProvider},
    store::AppStore,
};

use super::{resolve_workspace_path, truncate_output, validate_web_url, workspace_root};
pub(super) async fn image_generate_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("image_generate requires payload.prompt".into()))?;
    let provider = store
        .enabled_image_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled image provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "" => {
            openai_compatible_image_generate(store, run_id, &provider, prompt, payload).await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported image provider type: {other}"
        ))),
    }
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
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build image client: {error}")))?;
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
    if provider.provider_type == "echo" || provider.base_url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "text_to_speech requires an enabled OpenAI-compatible provider".into(),
        ));
    }
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "custom" | "" => {
            openai_compatible_text_to_speech(store, run_id, &provider, &text, payload).await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported text_to_speech provider type: {other}"
        ))),
    }
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
        "mp3" | "opus" | "aac" | "flac" | "wav" | "pcm" => Ok(format),
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
    if provider.provider_type == "echo" || provider.base_url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "transcribe_audio requires an enabled OpenAI-compatible provider".into(),
        ));
    }
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "custom" | "" => {
            openai_compatible_transcribe_audio(store, agent, run_id, &provider, &source, payload)
                .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported transcribe_audio provider type: {other}"
        ))),
    }
}

pub(super) async fn openai_compatible_transcribe_audio(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    provider: &LlmProvider,
    source: &str,
    payload: &Value,
) -> AppResult<String> {
    let (bytes, filename, mime_type, source_label) = transcribe_audio_bytes(agent, source).await?;
    let url = audio_transcriptions_url(provider)?;
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
    let file_part = reqwest::multipart::Part::bytes(bytes.clone())
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
    let transcript = extract_transcription_text(&body, &content_type)?;
    let artifact_path = store.save_tool_artifact(run_id, "transcribe_audio", &transcript)?;
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "source": source_label,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "artifactPath": artifact_path.to_string_lossy(),
        "transcript": transcript
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
        _ => "bin",
    }
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
    let provider = store
        .enabled_vision_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled vision provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "" => {
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
    let provider = store
        .enabled_vision_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled vision provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "" => {
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
