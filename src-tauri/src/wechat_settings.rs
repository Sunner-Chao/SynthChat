use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use base64::{engine::general_purpose, Engine as _};
use image::codecs::jpeg::JpegEncoder;
use md5::{Digest, Md5};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::{
    agent,
    error::{AppError, AppResult},
    models::{new_id, now_iso, ChatMessage, Persona, SendChatRequest},
    store::AppStore,
};

const DEFAULT_WECHAT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const LEGACY_BAD_WECHAT_BASE_URL: &str = "http://127.0.0.1:5030";
const WECHAT_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const WECHAT_IMAGE_MEDIA_TYPE: i64 = 1;
const WECHAT_FILE_MEDIA_TYPE: i64 = 3;
const WECHAT_VOICE_MEDIA_TYPE: i64 = 4;
const WECHAT_VOICE_ITEM_TYPE: i64 = 3;
const WECHAT_FILE_ITEM_TYPE: i64 = 4;
const WECHAT_IMAGE_MAX_BYTES: u64 = 20 * 1024 * 1024;
const WECHAT_FILE_MAX_BYTES: u64 = 100 * 1024 * 1024;
const WECHAT_VOICE_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_WECHAT_CHAT_THREAD_STACK_SIZE: usize = 64 * 1024 * 1024;
const MIN_WECHAT_CHAT_THREAD_STACK_SIZE: usize = 16 * 1024 * 1024;
const MAX_WECHAT_CHAT_THREAD_STACK_SIZE: usize = 256 * 1024 * 1024;
const WECHAT_QR_STATUS_TIMEOUT_SECONDS: u64 = 35;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountConfig {
    pub id: String,
    pub note: String,
    pub linked_persona: String,
    pub online: bool,
    pub created_at: String,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub ilink_user_id: String,
    #[serde(default)]
    pub get_updates_buf: String,
    #[serde(default)]
    pub login_base_url: String,
    #[serde(default)]
    pub last_login_at: String,
    #[serde(default)]
    pub last_wechat_user_id: String,
    #[serde(default)]
    pub last_context_token: String,
    #[serde(default)]
    pub last_inbound_at: String,
    #[serde(default)]
    pub raw_login_status: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatConfig {
    pub base_url: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatQrStartResult {
    pub qrcode: String,
    pub qr_image: Option<String>,
    pub base_url: String,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatQrStatusResult {
    pub status: String,
    pub message: Option<String>,
    pub account: Option<AccountConfig>,
    pub host: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatLinkSummary {
    pub account_id: String,
    pub persona_id: String,
    pub persona_name: String,
    pub account_note: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatInboundResult {
    pub messages: Vec<Value>,
    pub delivered: bool,
    pub delivery_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatPollResult {
    pub account: AccountConfig,
    pub processed: Vec<WechatProcessedInbound>,
    pub received_count: usize,
    pub skipped_count: usize,
    pub updated_buffer: bool,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatProcessedInbound {
    pub user_id: String,
    pub text: String,
    pub conversation_id: Option<String>,
    pub delivered: bool,
    pub delivery_error: Option<String>,
}

#[derive(Debug, Clone)]
struct WechatCdnImageInfo {
    encrypt_query_param: String,
    aes_key: String,
    mid_size: u64,
}

#[derive(Debug, Clone)]
struct WechatCdnFileInfo {
    encrypt_query_param: String,
    aes_key: String,
    file_name: String,
    length: u64,
}

#[derive(Debug, Clone)]
struct WechatCdnVoiceInfo {
    encrypt_query_param: String,
    aes_key: String,
    encode_type: i64,
    bits_per_sample: i64,
    sample_rate: i64,
    playtime_ms: i64,
}

#[derive(Debug, Clone)]
struct WechatInboundMedia {
    id: String,
    path: String,
    mime_type: String,
    label: String,
}

#[derive(Debug, Clone, Default)]
pub struct WechatInboundExtras {
    pub raw_message: Option<Value>,
    pub attachments: Vec<Value>,
}

pub fn data_path(name: &str) -> AppResult<PathBuf> {
    let base = crate::state_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(base.join(name))
}

fn legacy_data_path_candidates(name: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("synthchat-data").join(name));
            if let Some(grandparent) = parent.parent() {
                candidates.push(grandparent.join("synthchat-data").join(name));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("synthchat-data").join(name));
        candidates.push(
            cwd.join("src-tauri")
                .join("target")
                .join("debug")
                .join("synthchat-data")
                .join(name),
        );
        candidates.push(
            cwd.join("target")
                .join("debug")
                .join("synthchat-data")
                .join(name),
        );
    }
    candidates
}

fn file_has_non_whitespace_bytes(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => bytes.iter().any(|byte| !byte.is_ascii_whitespace()),
        Err(_) => false,
    }
}

fn maybe_restore_legacy_data_file(name: &str, target: &Path) {
    if file_has_non_whitespace_bytes(target) {
        return;
    }
    for candidate in legacy_data_path_candidates(name) {
        if candidate == target || !candidate.exists() || !file_has_non_whitespace_bytes(&candidate)
        {
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::copy(&candidate, target);
        break;
    }
}

fn read_json<T>(name: &str, fallback: T) -> AppResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    let path = data_path(name)?;
    maybe_restore_legacy_data_file(name, &path);
    if !path.exists() {
        return Ok(fallback);
    }
    let bytes = fs::read(path)?;
    if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(fallback);
    }
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(error) => {
            eprintln!(
                "SynthChat wechat settings ignored invalid JSON at {}: {}",
                data_path(name)?.display(),
                error
            );
            Ok(fallback)
        }
    }
}

fn write_json<T>(name: &str, value: &T) -> AppResult<()>
where
    T: Serialize,
{
    let path = data_path(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn default_wechat_config() -> WechatConfig {
    WechatConfig {
        base_url: std::env::var("SYNTHCHAT_WECHAT_BASE_URL")
            .or_else(|_| std::env::var("SYNTHCHAT_ILINK_BASE_URL"))
            .unwrap_or_else(|_| DEFAULT_WECHAT_BASE_URL.to_string()),
        timeout_seconds: std::env::var("SYNTHCHAT_WECHAT_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(35),
    }
}

fn should_replace_with_default_base_url(base_url: &str) -> bool {
    let trimmed = base_url.trim().trim_end_matches('/');
    trimmed.is_empty() || trimmed == LEGACY_BAD_WECHAT_BASE_URL
}

fn normalize_base_url(base_url: &str) -> AppResult<String> {
    let trimmed = base_url.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "wechat baseUrl is required; set it in settings or SYNTHCHAT_WECHAT_BASE_URL".into(),
        ));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(AppError::BadRequest(
            "wechat baseUrl must start with http:// or https://".into(),
        ));
    }
    Ok(trimmed)
}

fn common_ilink_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "iLink-App-Id",
        reqwest::header::HeaderValue::from_static("bot"),
    );
    headers.insert(
        "iLink-App-ClientVersion",
        reqwest::header::HeaderValue::from_static("132097"),
    );
    headers
}

fn ilink_headers_with_token(bot_token: &str) -> AppResult<reqwest::header::HeaderMap> {
    let mut headers = common_ilink_headers();
    let token = bot_token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest(
            "wechat account botToken is missing; scan login again".into(),
        ));
    }
    headers.insert(
        "Content-Type",
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        "AuthorizationType",
        reqwest::header::HeaderValue::from_static("ilink_bot_token"),
    );
    let value =
        reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            AppError::BadRequest(
                "wechat account botToken contains invalid header characters".into(),
            )
        })?;
    headers.insert("Authorization", value);
    Ok(headers)
}

fn value_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::to_string)
        .or_else(|| current.as_i64().map(|number| number.to_string()))
        .or_else(|| current.as_u64().map(|number| number.to_string()))
}

fn first_value_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| value_string(value, path))
}

fn value_as_non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .filter(|text| !text.trim().is_empty())
}

fn recursive_value_string(value: &Value, keys: &[&str]) -> Option<String> {
    fn walk(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
        if depth > 16 {
            return None;
        }
        match value {
            Value::Object(map) => {
                for key in keys {
                    if let Some(found) = map.get(*key).and_then(value_as_non_empty_string) {
                        return Some(found);
                    }
                }
                map.values().find_map(|item| walk(item, keys, depth + 1))
            }
            Value::Array(items) => items.iter().find_map(|item| walk(item, keys, depth + 1)),
            _ => None,
        }
    }
    walk(value, keys, 0)
}

fn image_data_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with("data:image/") {
        Some(trimmed.to_string())
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        Some(format!("data:image/png;base64,{trimmed}"))
    }
}

fn base64_image_data_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let bytes = general_purpose::STANDARD.decode(trimmed).ok()?;
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    Some(format!("data:{mime};base64,{trimmed}"))
}

fn qr_image_from_content(content: &str) -> AppResult<String> {
    let trimmed = content.trim();
    if let Some(data_url) = image_data_url(trimmed).filter(|_| trimmed.starts_with("data:image/")) {
        return Ok(data_url);
    }
    if let Some(data_url) = base64_image_data_url(trimmed) {
        return Ok(data_url);
    }
    generate_qr_svg_data_url(trimmed)
}

fn generate_qr_svg_data_url(content: &str) -> AppResult<String> {
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|error| AppError::BadRequest(format!("failed to render QR code: {error}")))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(192, 192)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#111111"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        general_purpose::STANDARD.encode(svg.as_bytes())
    ))
}

fn wechat_http_error(context: &str, error: reqwest::Error) -> AppError {
    AppError::BadRequest(format!("{context}: {error}"))
}

fn wechat_base_info() -> Value {
    json!({
        "channel_version": std::env::var("SYNTHCHAT_WECHAT_CHANNEL_VERSION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "2.4.1".to_string()),
    })
}

fn sendmessage_payload(msg: Value) -> Value {
    json!({
        "msg": msg,
        "base_info": wechat_base_info(),
    })
}

fn ensure_wechat_sendmessage_ok(raw: &Value, operation: &str) -> AppResult<()> {
    let code = raw
        .get("errcode")
        .and_then(Value::as_i64)
        .or_else(|| raw.get("ret").and_then(Value::as_i64))
        .unwrap_or(0);
    if code == 0 {
        return Ok(());
    }
    let errmsg = first_value_string(
        raw,
        &[
            &["errmsg"],
            &["err_msg"],
            &["message"],
            &["msg"],
            &["data", "errmsg"],
            &["data", "message"],
        ],
    )
    .unwrap_or_default();
    Err(AppError::BadRequest(format!(
        "{operation} failed code={code}: {errmsg}"
    )))
}

fn wechat_client_id() -> String {
    format!("synthchat-weixin-{}", new_id("client"))
}

fn md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn bytes_to_lower_hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn wechat_media_aes_key(aes_key: &[u8; 16]) -> String {
    general_purpose::STANDARD.encode(bytes_to_lower_hex(aes_key).as_bytes())
}

fn wechat_upload_filekey() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes_to_lower_hex(&bytes)
}

fn aes_128_ecb_pkcs7_encrypt(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let pad_len = 16 - (data.len() % 16);
    let mut padded = Vec::with_capacity(data.len() + pad_len);
    padded.extend_from_slice(data);
    padded.extend(std::iter::repeat(pad_len as u8).take(pad_len));
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for chunk in padded.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    padded
}

fn aes_128_ecb_pkcs7_decrypt(data: &[u8], key: &[u8; 16]) -> AppResult<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(AppError::BadRequest(
            "CDN encrypted media has invalid AES block length".to_string(),
        ));
    }
    let mut output = data.to_vec();
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for chunk in output.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    let pad_len = output
        .last()
        .copied()
        .ok_or_else(|| AppError::BadRequest("CDN decrypted media is empty".to_string()))?
        as usize;
    if pad_len == 0 || pad_len > 16 || pad_len > output.len() {
        return Err(AppError::BadRequest(
            "CDN decrypted media has invalid PKCS7 padding".to_string(),
        ));
    }
    let start = output.len() - pad_len;
    if !output[start..].iter().all(|byte| *byte as usize == pad_len) {
        return Err(AppError::BadRequest(
            "CDN decrypted media padding mismatch".to_string(),
        ));
    }
    output.truncate(start);
    Ok(output)
}

fn parse_wechat_aes_key(aes_key_b64: &str) -> AppResult<[u8; 16]> {
    let decoded = general_purpose::STANDARD
        .decode(aes_key_b64.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid media aes_key: {error}")))?;
    let key_bytes = if decoded.len() == 16 {
        decoded
    } else {
        let text = String::from_utf8_lossy(&decoded);
        let hex = text.trim();
        if hex.len() != 32 {
            return Err(AppError::BadRequest(format!(
                "media aes_key must decode to 16 raw bytes or 32-char hex, got {} bytes",
                decoded.len()
            )));
        }
        let mut bytes = Vec::with_capacity(16);
        for index in (0..hex.len()).step_by(2) {
            let byte = u8::from_str_radix(&hex[index..index + 2], 16).map_err(|error| {
                AppError::BadRequest(format!("invalid media aes_key hex: {error}"))
            })?;
            bytes.push(byte);
        }
        bytes
    };
    key_bytes
        .try_into()
        .map_err(|_| AppError::BadRequest("media aes_key length is not 16 bytes".to_string()))
}

fn wechat_cdn_upload_url(upload_raw: &Value, cdn_base: &str, filekey: &str) -> Option<String> {
    if let Some(upload_full_url) = first_value_string(
        upload_raw,
        &[
            &["upload_full_url"],
            &["uploadFullUrl"],
            &["data", "upload_full_url"],
            &["data", "uploadFullUrl"],
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    {
        return Some(upload_full_url);
    }

    let upload_param = first_value_string(
        upload_raw,
        &[
            &["upload_param"],
            &["uploadParam"],
            &["encrypted_query_param"],
            &["encrypt_query_param"],
            &["data", "upload_param"],
            &["data", "uploadParam"],
            &["data", "encrypted_query_param"],
            &["data", "encrypt_query_param"],
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())?;
    let base = format!("{}/upload", cdn_base.trim_end_matches('/'));
    let mut url = reqwest::Url::parse(&base).ok()?;
    url.query_pairs_mut()
        .append_pair("encrypted_query_param", &upload_param)
        .append_pair("filekey", filekey);
    Some(url.to_string())
}

fn detect_wechat_image_mime(data: &[u8], path: &Path) -> AppResult<&'static str> {
    let mime = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "image/gif"
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => {
                return Err(AppError::BadRequest(format!(
                    "unsupported image format for WeChat: {}",
                    path.to_string_lossy()
                )))
            }
        }
    };
    Ok(mime)
}

fn wechat_outbound_image_payload(data: &[u8], path: &Path) -> AppResult<(Vec<u8>, &'static str)> {
    let mime = detect_wechat_image_mime(data, path)?;
    let normalize_non_jpeg = std::env::var("SYNTHCHAT_WECHAT_NORMALIZE_NON_JPEG_IMAGES")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no"
            )
        })
        .unwrap_or(true);
    if mime == "image/jpeg" || mime == "image/gif" || !normalize_non_jpeg {
        return Ok((data.to_vec(), mime));
    }
    let decoded = image::load_from_memory(data).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to decode image for WeChat-compatible upload: {error}"
        ))
    })?;
    let rgb = decoded.to_rgb8();
    let quality = std::env::var("SYNTHCHAT_WECHAT_JPEG_QUALITY")
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .unwrap_or(92)
        .clamp(60, 100);
    let mut encoded = Vec::new();
    {
        let mut cursor = Cursor::new(&mut encoded);
        let mut encoder = JpegEncoder::new_with_quality(&mut cursor, quality);
        encoder.encode_image(&rgb).map_err(|error| {
            AppError::BadRequest(format!("failed to encode WeChat JPEG upload: {error}"))
        })?;
    }
    Ok((encoded, "image/jpeg"))
}

fn extension_for_image_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    if mime.starts_with("image/") {
        return Some(extension_for_image_mime(mime));
    }
    match mime {
        "application/pdf" => Some("pdf"),
        "text/plain" => Some("txt"),
        "application/json" => Some("json"),
        "application/zip" => Some("zip"),
        "audio/mpeg" => Some("mp3"),
        "audio/wav" => Some("wav"),
        "audio/amr" => Some("amr"),
        "audio/ogg" => Some("ogg"),
        _ => None,
    }
}

fn file_name_with_mime_extension(file_name: &str, mime_type: &str) -> String {
    let clean = safe_media_file_name(file_name, "wechat-file");
    if Path::new(&clean).extension().is_some() {
        return clean;
    }
    extension_for_mime(mime_type)
        .map(|extension| format!("{clean}.{extension}"))
        .unwrap_or(clean)
}

fn mime_from_file_name(file_name: &str) -> String {
    match Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".to_string(),
        Some("jpg" | "jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("bmp") => "image/bmp".to_string(),
        Some("pdf") => "application/pdf".to_string(),
        Some("txt" | "md" | "csv" | "log") => "text/plain".to_string(),
        Some("json") => "application/json".to_string(),
        Some("zip") => "application/zip".to_string(),
        Some("doc") => "application/msword".to_string(),
        Some("docx") => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        Some("xls") => "application/vnd.ms-excel".to_string(),
        Some("xlsx") => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
        }
        Some("mp3") => "audio/mpeg".to_string(),
        Some("wav") => "audio/wav".to_string(),
        Some("amr") => "audio/amr".to_string(),
        Some("silk") => "audio/silk".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn wechat_voice_extension(encode_type: i64) -> &'static str {
    match encode_type {
        4 => "silk",
        5 => "amr",
        6 => "silk",
        7 => "mp3",
        8 => "spx",
        _ => "bin",
    }
}

fn wechat_voice_mime_type(encode_type: i64) -> &'static str {
    match encode_type {
        4 => "audio/silk",
        5 => "audio/amr",
        6 => "audio/silk",
        7 => "audio/mpeg",
        8 => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn safe_media_file_name(raw: &str, fallback: &str) -> String {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let cleaned = name
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.trim_matches(['.', '_', '-', ' ']).is_empty() {
        fallback.to_string()
    } else {
        truncate_file_name(&cleaned, 180)
    }
}

fn truncate_file_name(name: &str, limit: usize) -> String {
    if name.chars().count() <= limit {
        return name.to_string();
    }
    let path = Path::new(name);
    let extension = path.extension().and_then(|value| value.to_str());
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    if let Some(extension) = extension.filter(|value| !value.is_empty()) {
        let suffix = format!(".{extension}");
        let stem_limit = limit.saturating_sub(suffix.chars().count()).max(1);
        format!(
            "{}{}",
            stem.chars().take(stem_limit).collect::<String>(),
            suffix
        )
    } else {
        name.chars().take(limit).collect()
    }
}

fn is_image_file_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    )
}

fn is_audio_file_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("silk" | "amr" | "mp3" | "wav" | "ogg" | "opus")
    )
}

fn line_is_wechat_media_directive(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains("[media attached:")
        || trimmed
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("MEDIA:"))
}

fn extract_media_tag_path_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_matches('`');
    let (prefix, rest) = trimmed.split_at(trimmed.get(..6)?.len());
    if !prefix.eq_ignore_ascii_case("MEDIA:") {
        return None;
    }
    let path = rest
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches(['，', '。', ',', '.', ';', '；'])
        .to_string();
    if PathBuf::from(&path).is_file() {
        Some(path)
    } else {
        None
    }
}

fn extract_media_attached_path_from_line(line: &str) -> Option<String> {
    let cleaned = line
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'');
    let chars = cleaned.char_indices().collect::<Vec<_>>();
    let start = chars.windows(3).find_map(|window| {
        let (_, drive) = window[0];
        let (_, colon) = window[1];
        let (_, slash) = window[2];
        if drive.is_ascii_alphabetic() && colon == ':' && (slash == '\\' || slash == '/') {
            Some(window[0].0)
        } else {
            None
        }
    })?;
    let rest = &cleaned[start..];
    let end = rest
        .char_indices()
        .find_map(|(index, ch)| {
            if ch == '`' || ch == '"' || ch == '\'' || ch == '<' || ch == '>' || ch == ')' {
                Some(index)
            } else {
                None
            }
        })
        .unwrap_or(rest.len());
    let path = rest[..end]
        .trim()
        .split_once(" (")
        .map(|(path, _)| path)
        .unwrap_or_else(|| rest[..end].trim())
        .trim_end_matches(['，', '。', ',', '.', ';', '；'])
        .to_string();
    if PathBuf::from(&path).is_file() {
        Some(path)
    } else {
        None
    }
}

fn extract_media_path_from_line(line: &str) -> Option<String> {
    extract_media_tag_path_from_line(line).or_else(|| extract_media_attached_path_from_line(line))
}

fn extract_wechat_image_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        if let Some(path) =
            extract_media_path_from_line(line).filter(|path| is_image_file_path(path))
        {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn extract_wechat_file_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        if let Some(path) = extract_media_path_from_line(line)
            .filter(|path| !is_image_file_path(path) && !is_audio_file_path(path))
        {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn extract_wechat_voice_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        if let Some(path) =
            extract_media_path_from_line(line).filter(|path| is_audio_file_path(path))
        {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn strip_wechat_media_marker_lines(text: &str) -> String {
    text.lines()
        .filter(|line| !line_is_wechat_media_directive(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

async fn upload_wechat_image(
    account: &AccountConfig,
    to_user_id: &str,
    image_path: &str,
) -> AppResult<WechatCdnImageInfo> {
    let path = PathBuf::from(image_path);
    if !path.is_file() {
        return Err(AppError::NotFound(format!(
            "image file not found: {}",
            path.to_string_lossy()
        )));
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() == 0 || metadata.len() > WECHAT_IMAGE_MAX_BYTES {
        return Err(AppError::BadRequest(format!(
            "image file must be between 1 byte and {} MiB: {}",
            WECHAT_IMAGE_MAX_BYTES / 1024 / 1024,
            path.to_string_lossy()
        )));
    }
    let raw_plain = fs::read(&path)?;
    let (plain, _upload_mime) = wechat_outbound_image_payload(&raw_plain, &path)?;
    let raw_md5 = md5_hex(&plain);
    let mut aes_key = [0_u8; 16];
    rand::rng().fill_bytes(&mut aes_key);
    let aes_key_hex = bytes_to_lower_hex(&aes_key);
    let media_aes_key = wechat_media_aes_key(&aes_key);
    let encrypted = aes_128_ecb_pkcs7_encrypt(&plain, &aes_key);
    let encrypted_len = encrypted.len();
    let filekey = wechat_upload_filekey();
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let upload_payload = json!({
        "filekey": filekey,
        "media_type": WECHAT_IMAGE_MEDIA_TYPE,
        "to_user_id": to_user_id,
        "rawsize": plain.len(),
        "rawfilemd5": raw_md5,
        "filesize": encrypted_len,
        "aeskey": aes_key_hex,
        "no_need_thumb": true,
        "base_info": wechat_base_info()
    });
    let upload_raw: Value = client
        .post(format!("{base_url}/ilink/bot/getuploadurl"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&upload_payload)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat image upload URL", error))?
        .error_for_status()
        .map_err(|error| {
            wechat_http_error("wechat getuploadurl endpoint returned an error", error)
        })?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat upload response", error))?;
    let errcode = upload_raw
        .get("errcode")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if errcode != 0 {
        let errmsg = first_value_string(&upload_raw, &[&["errmsg"], &["message"], &["msg"]])
            .unwrap_or_default();
        return Err(AppError::BadRequest(format!(
            "getUploadUrl(image) errcode={errcode}: {errmsg}"
        )));
    }
    let cdn_base = std::env::var("SYNTHCHAT_WECHAT_CDN_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| WECHAT_CDN_BASE_URL.to_string());
    let upload_url = wechat_cdn_upload_url(&upload_raw, &cdn_base, &filekey).ok_or_else(|| {
        AppError::BadRequest(format!(
            "getUploadUrl(image) response missing upload URL/param: {upload_raw}"
        ))
    })?;
    let upload_response = client
        .post(upload_url)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(encrypted)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to upload wechat image CDN payload", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat CDN upload returned an error", error))?;
    let encrypted_param = upload_response
        .headers()
        .get("x-encrypted-param")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AppError::BadRequest("CDN image upload missing x-encrypted-param".to_string())
        })?;
    Ok(WechatCdnImageInfo {
        encrypt_query_param: encrypted_param,
        aes_key: media_aes_key,
        mid_size: encrypted_len as u64,
    })
}

async fn upload_wechat_file(
    account: &AccountConfig,
    to_user_id: &str,
    file_path: &str,
) -> AppResult<WechatCdnFileInfo> {
    let path = PathBuf::from(file_path);
    if !path.is_file() {
        return Err(AppError::NotFound(format!(
            "file not found: {}",
            path.to_string_lossy()
        )));
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() == 0 || metadata.len() > WECHAT_FILE_MAX_BYTES {
        return Err(AppError::BadRequest(format!(
            "file must be between 1 byte and {} MiB: {}",
            WECHAT_FILE_MAX_BYTES / 1024 / 1024,
            path.to_string_lossy()
        )));
    }
    let plain = fs::read(&path)?;
    let raw_md5 = md5_hex(&plain);
    let mut aes_key = [0_u8; 16];
    rand::rng().fill_bytes(&mut aes_key);
    let aes_key_hex = bytes_to_lower_hex(&aes_key);
    let media_aes_key = wechat_media_aes_key(&aes_key);
    let encrypted = aes_128_ecb_pkcs7_encrypt(&plain, &aes_key);
    let filekey = wechat_upload_filekey();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("file")
        .to_string();
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let upload_payload = json!({
        "filekey": filekey,
        "media_type": WECHAT_FILE_MEDIA_TYPE,
        "to_user_id": to_user_id,
        "rawsize": plain.len(),
        "rawfilemd5": raw_md5,
        "filesize": encrypted.len(),
        "aeskey": aes_key_hex,
        "no_need_thumb": true,
        "base_info": wechat_base_info()
    });
    let upload_raw: Value = client
        .post(format!("{base_url}/ilink/bot/getuploadurl"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&upload_payload)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat file upload URL", error))?
        .error_for_status()
        .map_err(|error| {
            wechat_http_error("wechat getuploadurl endpoint returned an error", error)
        })?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat file upload response", error))?;
    let errcode = upload_raw
        .get("errcode")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if errcode != 0 {
        let errmsg = first_value_string(&upload_raw, &[&["errmsg"], &["message"], &["msg"]])
            .unwrap_or_default();
        return Err(AppError::BadRequest(format!(
            "getUploadUrl(file) errcode={errcode}: {errmsg}"
        )));
    }
    let cdn_base = std::env::var("SYNTHCHAT_WECHAT_CDN_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| WECHAT_CDN_BASE_URL.to_string());
    let upload_url = wechat_cdn_upload_url(&upload_raw, &cdn_base, &filekey).ok_or_else(|| {
        AppError::BadRequest(format!(
            "getUploadUrl(file) response missing upload URL/param: {upload_raw}"
        ))
    })?;
    let upload_response = client
        .post(upload_url)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(encrypted)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to upload wechat file CDN payload", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat CDN upload returned an error", error))?;
    let encrypted_param = upload_response
        .headers()
        .get("x-encrypted-param")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AppError::BadRequest("CDN file upload missing x-encrypted-param".to_string())
        })?;
    Ok(WechatCdnFileInfo {
        encrypt_query_param: encrypted_param,
        aes_key: media_aes_key,
        file_name,
        length: plain.len() as u64,
    })
}

fn wechat_voice_metadata(path: &Path, bytes: &[u8]) -> (i64, i64, i64, i64) {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("silk") => (4, 16_000, 16, 0),
        Some("mp3") => (7, 24_000, 16, 0),
        Some("amr") => (5, 8_000, 16, 0),
        Some("ogg" | "opus") => (8, 24_000, 16, 0),
        Some("wav") => wav_voice_metadata(bytes).unwrap_or((1, 16_000, 16, 0)),
        _ => (7, 24_000, 16, 0),
    }
}

fn wav_voice_metadata(bytes: &[u8]) -> Option<(i64, i64, i64, i64)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]).max(1) as u64;
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]) as u64;
    let bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]) as u64;
    let data_bytes = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as u64;
    let bytes_per_second = sample_rate
        .saturating_mul(channels)
        .saturating_mul(bits_per_sample)
        / 8;
    let playtime_ms = if bytes_per_second > 0 {
        data_bytes.saturating_mul(1000) / bytes_per_second
    } else {
        0
    };
    Some((
        1,
        sample_rate as i64,
        bits_per_sample as i64,
        playtime_ms as i64,
    ))
}

async fn upload_wechat_voice(
    account: &AccountConfig,
    to_user_id: &str,
    voice_path: &str,
) -> AppResult<WechatCdnVoiceInfo> {
    let path = PathBuf::from(voice_path);
    if !path.is_file() {
        return Err(AppError::NotFound(format!(
            "voice file not found: {}",
            path.to_string_lossy()
        )));
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() == 0 || metadata.len() > WECHAT_VOICE_MAX_BYTES {
        return Err(AppError::BadRequest(format!(
            "voice file must be between 1 byte and {} MiB: {}",
            WECHAT_VOICE_MAX_BYTES / 1024 / 1024,
            path.to_string_lossy()
        )));
    }
    let plain = fs::read(&path)?;
    let raw_md5 = md5_hex(&plain);
    let mut aes_key = [0_u8; 16];
    rand::rng().fill_bytes(&mut aes_key);
    let aes_key_hex = bytes_to_lower_hex(&aes_key);
    let media_aes_key = wechat_media_aes_key(&aes_key);
    let encrypted = aes_128_ecb_pkcs7_encrypt(&plain, &aes_key);
    let filekey = wechat_upload_filekey();
    let (encode_type, sample_rate, bits_per_sample, playtime_ms) =
        wechat_voice_metadata(&path, &plain);
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let upload_payload = json!({
        "filekey": filekey,
        "media_type": WECHAT_VOICE_MEDIA_TYPE,
        "to_user_id": to_user_id,
        "rawsize": plain.len(),
        "rawfilemd5": raw_md5,
        "filesize": encrypted.len(),
        "aeskey": aes_key_hex,
        "no_need_thumb": true,
        "base_info": wechat_base_info()
    });
    let upload_raw: Value = client
        .post(format!("{base_url}/ilink/bot/getuploadurl"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&upload_payload)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat voice upload URL", error))?
        .error_for_status()
        .map_err(|error| {
            wechat_http_error("wechat getuploadurl endpoint returned an error", error)
        })?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat voice upload response", error))?;
    let errcode = upload_raw
        .get("errcode")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if errcode != 0 {
        let errmsg = first_value_string(&upload_raw, &[&["errmsg"], &["message"], &["msg"]])
            .unwrap_or_default();
        return Err(AppError::BadRequest(format!(
            "getUploadUrl(voice) errcode={errcode}: {errmsg}"
        )));
    }
    let cdn_base = std::env::var("SYNTHCHAT_WECHAT_CDN_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| WECHAT_CDN_BASE_URL.to_string());
    let upload_url = wechat_cdn_upload_url(&upload_raw, &cdn_base, &filekey).ok_or_else(|| {
        AppError::BadRequest(format!(
            "getUploadUrl(voice) response missing upload URL/param: {upload_raw}"
        ))
    })?;
    let upload_response = client
        .post(upload_url)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(encrypted)
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to upload wechat voice CDN payload", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat CDN upload returned an error", error))?;
    let encrypted_param = upload_response
        .headers()
        .get("x-encrypted-param")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AppError::BadRequest("CDN voice upload missing x-encrypted-param".to_string())
        })?;
    Ok(WechatCdnVoiceInfo {
        encrypt_query_param: encrypted_param,
        aes_key: media_aes_key,
        encode_type,
        bits_per_sample,
        sample_rate,
        playtime_ms,
    })
}

async fn send_wechat_text_message(
    account: &AccountConfig,
    to_user_id: &str,
    text: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let mut msg = json!({
        "to_user_id": to_user_id,
        "from_user_id": "",
        "client_id": wechat_client_id(),
        "message_type": 2,
        "message_state": 2,
        "item_list": [{
            "type": 1,
            "text_item": { "text": text }
        }]
    });
    if let Some(token) = context_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        msg["context_token"] = Value::String(token.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .post(format!("{base_url}/ilink/bot/sendmessage"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&sendmessage_payload(msg))
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to send wechat text message", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat sendmessage endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat sendmessage response", error))?;
    ensure_wechat_sendmessage_ok(&raw, "sendMessage")?;
    Ok(raw)
}

async fn send_wechat_text_message_with_retry(
    account: &AccountConfig,
    to_user_id: &str,
    text: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    match send_wechat_text_message(account, to_user_id, text, context_token).await {
        Ok(raw) => Ok(raw),
        Err(_) if context_token.is_some() => {
            send_wechat_text_message(account, to_user_id, text, None).await
        }
        Err(error) => Err(error),
    }
}

async fn send_wechat_image_message(
    account: &AccountConfig,
    to_user_id: &str,
    image_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    let cdn_info = upload_wechat_image(account, to_user_id, image_path).await?;
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let mut msg = json!({
        "to_user_id": to_user_id,
        "from_user_id": "",
        "client_id": wechat_client_id(),
        "message_type": 2,
        "message_state": 2,
        "item_list": [{
            "type": 2,
            "image_item": {
                "media": {
                    "encrypt_query_param": cdn_info.encrypt_query_param,
                    "aes_key": cdn_info.aes_key,
                    "encrypt_type": 1
                },
                "mid_size": cdn_info.mid_size
            }
        }]
    });
    if let Some(token) = context_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        msg["context_token"] = Value::String(token.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .post(format!("{base_url}/ilink/bot/sendmessage"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&sendmessage_payload(msg))
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to send wechat image message", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat sendmessage endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat image send response", error))?;
    ensure_wechat_sendmessage_ok(&raw, "sendImageMessage")?;
    Ok(raw)
}

async fn send_wechat_image_message_with_retry(
    account: &AccountConfig,
    to_user_id: &str,
    image_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    match send_wechat_image_message(account, to_user_id, image_path, context_token).await {
        Ok(raw) => Ok(raw),
        Err(_) if context_token.is_some() => {
            send_wechat_image_message(account, to_user_id, image_path, None).await
        }
        Err(error) => Err(error),
    }
}

async fn send_wechat_file_message(
    account: &AccountConfig,
    to_user_id: &str,
    file_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    let cdn_info = upload_wechat_file(account, to_user_id, file_path).await?;
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let mut msg = json!({
        "to_user_id": to_user_id,
        "from_user_id": "",
        "client_id": wechat_client_id(),
        "message_type": 2,
        "message_state": 2,
        "item_list": [{
            "type": WECHAT_FILE_ITEM_TYPE,
            "file_item": {
                "media": {
                    "encrypt_query_param": cdn_info.encrypt_query_param,
                    "aes_key": cdn_info.aes_key,
                    "encrypt_type": 1
                },
                "file_name": cdn_info.file_name,
                "len": cdn_info.length.to_string()
            }
        }]
    });
    if let Some(token) = context_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        msg["context_token"] = Value::String(token.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .post(format!("{base_url}/ilink/bot/sendmessage"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&sendmessage_payload(msg))
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to send wechat file message", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat sendmessage endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat file send response", error))?;
    ensure_wechat_sendmessage_ok(&raw, "sendFileMessage")?;
    Ok(raw)
}

async fn send_wechat_file_message_with_retry(
    account: &AccountConfig,
    to_user_id: &str,
    file_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    match send_wechat_file_message(account, to_user_id, file_path, context_token).await {
        Ok(raw) => Ok(raw),
        Err(_) if context_token.is_some() => {
            send_wechat_file_message(account, to_user_id, file_path, None).await
        }
        Err(error) => Err(error),
    }
}

async fn send_wechat_voice_message(
    account: &AccountConfig,
    to_user_id: &str,
    voice_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    let cdn_info = upload_wechat_voice(account, to_user_id, voice_path).await?;
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let mut msg = json!({
        "to_user_id": to_user_id,
        "from_user_id": "",
        "client_id": wechat_client_id(),
        "message_type": 2,
        "message_state": 2,
        "item_list": [{
            "type": WECHAT_VOICE_ITEM_TYPE,
            "voice_item": {
                "media": {
                    "encrypt_query_param": cdn_info.encrypt_query_param,
                    "aes_key": cdn_info.aes_key,
                    "encrypt_type": 1
                },
                "encode_type": cdn_info.encode_type,
                "bits_per_sample": cdn_info.bits_per_sample,
                "sample_rate": cdn_info.sample_rate,
                "playtime": cdn_info.playtime_ms
            }
        }]
    });
    if let Some(token) = context_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        msg["context_token"] = Value::String(token.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .post(format!("{base_url}/ilink/bot/sendmessage"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&sendmessage_payload(msg))
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to send wechat voice message", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat sendmessage endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat voice send response", error))?;
    ensure_wechat_sendmessage_ok(&raw, "sendVoiceMessage")?;
    Ok(raw)
}

async fn send_wechat_voice_message_with_retry(
    account: &AccountConfig,
    to_user_id: &str,
    voice_path: &str,
    context_token: Option<&str>,
) -> AppResult<Value> {
    match send_wechat_voice_message(account, to_user_id, voice_path, context_token).await {
        Ok(raw) => Ok(raw),
        Err(_) if context_token.is_some() => {
            send_wechat_voice_message(account, to_user_id, voice_path, None).await
        }
        Err(error) => Err(error),
    }
}

async fn download_wechat_cdn_media(
    client: &reqwest::Client,
    media: &Value,
    max_bytes: u64,
) -> AppResult<Vec<u8>> {
    let aes_key = first_value_string(media, &[&["aes_key"], &["aesKey"], &["aeskey"]])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("wechat media missing aes_key".to_string()))?;
    let key = parse_wechat_aes_key(&aes_key)?;
    let response =
        if let Some(full_url) = first_value_string(media, &[&["full_url"], &["fullUrl"]])
            .filter(|value| !value.trim().is_empty())
        {
            client.get(full_url).send().await
        } else {
            let encrypt_param = first_value_string(
                media,
                &[&["encrypt_query_param"], &["encryptQueryParam"], &["url"]],
            )
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("wechat media missing encrypt_query_param".to_string())
            })?;
            let cdn_base = std::env::var("SYNTHCHAT_WECHAT_CDN_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| WECHAT_CDN_BASE_URL.to_string());
            client
                .get(format!("{}/download", cdn_base.trim_end_matches('/')))
                .query(&[("encrypted_query_param", encrypt_param.as_str())])
                .send()
                .await
        }
        .map_err(|error| wechat_http_error("failed to download wechat CDN media", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat CDN download returned an error", error))?;
    let cipher = response
        .bytes()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat CDN media body", error))?;
    if cipher.len() as u64 > max_bytes {
        return Err(AppError::BadRequest(format!(
            "wechat media exceeds {} MiB",
            max_bytes / 1024 / 1024
        )));
    }
    aes_128_ecb_pkcs7_decrypt(&cipher, &key)
}

fn attachment_dir(store: &AppStore) -> AppResult<PathBuf> {
    let dir = store.data_dir().join("attachments");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn save_wechat_attachment(
    store: &AppStore,
    file_name: String,
    bytes: Vec<u8>,
    mime_type: String,
    label: String,
) -> AppResult<WechatInboundMedia> {
    let id = new_id("wechat_attachment");
    let clean_name = file_name_with_mime_extension(&file_name, &mime_type);
    let path = attachment_dir(store)?.join(format!("{id}-{clean_name}"));
    fs::write(&path, bytes)?;
    Ok(WechatInboundMedia {
        id,
        path: path.to_string_lossy().to_string(),
        mime_type,
        label,
    })
}

fn media_label_from_value(value: &Value, fallback: &str) -> String {
    first_value_string(
        value,
        &[
            &["fileName"],
            &["file_name"],
            &["filename"],
            &["name"],
            &["label"],
            &["title"],
            &["displayName"],
            &["display_name"],
            &["originalName"],
            &["original_name"],
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| fallback.to_string())
}

fn media_mime_from_value(value: &Value, label: &str) -> String {
    if let Some(mime) = first_value_string(
        value,
        &[
            &["mimeType"],
            &["mime_type"],
            &["contentType"],
            &["content_type"],
            &["mediaType"],
            &["media_type"],
            &["type"],
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| value.contains('/'))
    {
        return mime;
    }
    let from_label = mime_from_file_name(label);
    if from_label != "application/octet-stream" {
        return from_label;
    }
    first_value_string(
        value,
        &[
            &["dataUrl"],
            &["data_url"],
            &["base64"],
            &["data"],
            &["content"],
        ],
    )
    .and_then(|data| {
        let (bytes, detected_mime) = if data.trim_start().starts_with("data:") {
            decode_data_url_payload(&data)?
        } else {
            let bytes = general_purpose::STANDARD.decode(data.trim()).ok()?;
            (bytes, String::new())
        };
        if detected_mime.contains('/') && detected_mime != "application/octet-stream" {
            return Some(detected_mime);
        }
        detect_wechat_image_mime(&bytes, Path::new(label))
            .ok()
            .map(str::to_string)
    })
    .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn detect_local_media_mime(path: &Path, fallback_label: &str) -> String {
    let from_name = mime_from_file_name(
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(fallback_label),
    );
    if from_name != "application/octet-stream" {
        return from_name;
    }
    fs::read(path)
        .ok()
        .and_then(|bytes| detect_wechat_image_mime(&bytes, path).ok().map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn decode_data_url_payload(value: &str) -> Option<(Vec<u8>, String)> {
    let trimmed = value.trim();
    let (meta, payload) = trimmed.split_once(',')?;
    if !meta.starts_with("data:") || !meta.contains(";base64") {
        return None;
    }
    let mime = meta
        .trim_start_matches("data:")
        .split(';')
        .next()
        .filter(|mime| !mime.trim().is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = general_purpose::STANDARD.decode(payload.trim()).ok()?;
    Some((bytes, mime))
}

fn attachment_value_to_saved_media(store: &AppStore, value: &Value) -> Option<WechatInboundMedia> {
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str().map(str::trim).filter(|value| !value.is_empty()) {
        if text.starts_with("data:") {
            let (bytes, mime_type) = decode_data_url_payload(text)?;
            let label = file_name_with_mime_extension(&format!("wechat-{}", new_id("media")), &mime_type);
            return save_wechat_attachment(store, label.clone(), bytes, mime_type, label).ok();
        }
        let source_path = PathBuf::from(text);
        if !source_path.is_file() {
            return None;
        }
        let label = attachment_file_name_for_path(text);
        let bytes = fs::read(&source_path).ok()?;
        let mime_type = detect_local_media_mime(&source_path, &label);
        return save_wechat_attachment(store, label.clone(), bytes, mime_type, label).ok();
    }
    if let Some(path) = first_value_string(
        value,
        &[
            &["path"],
            &["filePath"],
            &["file_path"],
            &["localPath"],
            &["local_path"],
            &["sourcePath"],
            &["source_path"],
            &["tempPath"],
            &["temp_path"],
            &["thumbPath"],
            &["thumb_path"],
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    {
        let label = media_label_from_value(value, &attachment_file_name_for_path(&path));
        let source_path = PathBuf::from(&path);
        if !source_path.is_file() {
            return None;
        }
        let bytes = fs::read(&source_path).ok()?;
        let configured_mime = media_mime_from_value(value, &label);
        let mime_type = if configured_mime == "application/octet-stream" {
            detect_local_media_mime(&source_path, &label)
        } else {
            configured_mime
        };
        return save_wechat_attachment(store, label.clone(), bytes, mime_type, label).ok();
    }
    let label = media_label_from_value(value, "wechat-image");
    let data = first_value_string(
        value,
        &[
            &["dataUrl"],
            &["data_url"],
            &["base64"],
            &["data"],
            &["content"],
        ],
    )?;
    let (bytes, detected_mime) = if data.trim_start().starts_with("data:") {
        decode_data_url_payload(&data)?
    } else {
        let bytes = general_purpose::STANDARD.decode(data.trim()).ok()?;
        let mime = media_mime_from_value(value, &label);
        (bytes, mime)
    };
    let mime_type = media_mime_from_value(value, &label);
    let mime_type = if mime_type == "application/octet-stream" {
        detected_mime
    } else {
        mime_type
    };
    save_wechat_attachment(store, label.clone(), bytes, mime_type, label).ok()
}

fn collect_extra_attachment_values(value: &Value, output: &mut Vec<Value>, depth: usize) {
    if depth > 16 {
        return;
    }
    if attachment_value_to_saved_media_candidate(value) {
        output.push(value.clone());
    }
    match value {
        Value::Object(map) => {
            for key in [
                "attachments",
                "attachmentContexts",
                "attachment_contexts",
                "mediaFiles",
                "media_files",
                "files",
                "images",
                "imageFiles",
                "image_files",
                "fileList",
                "file_list",
            ] {
                if let Some(child) = map.get(key) {
                    collect_extra_attachment_values(child, output, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_extra_attachment_values(item, output, depth + 1);
            }
        }
        _ => {}
    }
}

fn attachment_value_to_saved_media_candidate(value: &Value) -> bool {
    if value.as_str().is_some_and(|text| {
        let trimmed = text.trim();
        trimmed.starts_with("data:image/")
            || trimmed.starts_with("data:application/")
            || PathBuf::from(trimmed).is_file()
    }) {
        return true;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    let has_path = [
        "path",
        "filePath",
        "file_path",
        "localPath",
        "local_path",
        "sourcePath",
        "source_path",
        "tempPath",
        "temp_path",
        "thumbPath",
        "thumb_path",
    ]
    .iter()
    .any(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|text| !text.is_empty() && PathBuf::from(text).is_file())
    });
    let has_inline_data = ["dataUrl", "data_url", "base64", "base64Data", "base64_data"]
        .iter()
        .any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|text| text.starts_with("data:") || looks_like_base64_media(text))
    });
    has_path || has_inline_data
}

fn looks_like_base64_media(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.len() > 64
        && trimmed.len() % 4 == 0
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
}

fn raw_wechat_message_has_media(raw_msg: &Value) -> bool {
    let has_item_media = raw_msg
        .get("item_list")
        .or_else(|| raw_msg.get("itemList"))
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_i64).unwrap_or_default(),
                    2 | WECHAT_FILE_ITEM_TYPE | WECHAT_VOICE_ITEM_TYPE
                ) || item.get("image_item").is_some()
                    || item.get("imageItem").is_some()
                    || item.get("file_item").is_some()
                    || item.get("fileItem").is_some()
                    || item.get("voice_item").is_some()
                    || item.get("voiceItem").is_some()
            })
        });
    if has_item_media {
        return true;
    }
    let mut extras = Vec::new();
    collect_extra_attachment_values(raw_msg, &mut extras, 0);
    !extras.is_empty()
}

fn attachment_file_name_for_path(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "wechat-file".to_string())
}

fn media_from_extra_attachments(store: &AppStore, attachments: &[Value]) -> Vec<WechatInboundMedia> {
    let mut media = Vec::new();
    for attachment in attachments {
        if let Some(item) = attachment_value_to_saved_media(store, attachment) {
            media.push(item);
        }
    }
    media
}

async fn extract_and_save_wechat_media(
    store: &AppStore,
    account: &AccountConfig,
    raw_msg: &Value,
) -> Vec<WechatInboundMedia> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("SynthChat wechat media client failed: {error}");
            return Vec::new();
        }
    };
    let items = raw_msg
        .get("item_list")
        .or_else(|| raw_msg.get("itemList"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut saved = Vec::new();
    for item in items {
        let item_type = item.get("type").and_then(Value::as_i64).unwrap_or_default();
        if item_type == 2 {
            let Some(media) = item
                .get("image_item")
                .or_else(|| item.get("imageItem"))
                .and_then(|value| value.get("media"))
            else {
                continue;
            };
            match download_wechat_cdn_media(&client, media, WECHAT_IMAGE_MAX_BYTES).await {
                Ok(bytes) => {
                    let fallback = PathBuf::from("wechat-image.png");
                    let mime = detect_wechat_image_mime(&bytes, &fallback).unwrap_or("image/png");
                    let name = format!(
                        "wechat-{}-{}.{}",
                        account.id,
                        new_id("media"),
                        extension_for_image_mime(mime)
                    );
                    match save_wechat_attachment(
                        store,
                        name,
                        bytes,
                        mime.to_string(),
                        "图片".to_string(),
                    ) {
                        Ok(media) => saved.push(media),
                        Err(error) => eprintln!("SynthChat wechat image save failed: {error}"),
                    }
                }
                Err(error) => eprintln!("SynthChat wechat image download failed: {error}"),
            }
        } else if item_type == WECHAT_FILE_ITEM_TYPE {
            let Some(file_item) = item.get("file_item").or_else(|| item.get("fileItem")) else {
                continue;
            };
            let Some(media) = file_item.get("media") else {
                continue;
            };
            let file_name = first_value_string(
                file_item,
                &[
                    &["file_name"],
                    &["fileName"],
                    &["name"],
                    &["title"],
                    &["display_name"],
                ],
            )
            .unwrap_or_else(|| "file".to_string());
            match download_wechat_cdn_media(&client, media, WECHAT_FILE_MAX_BYTES).await {
                Ok(bytes) => {
                    let clean_name = safe_media_file_name(&file_name, "file");
                    match save_wechat_attachment(
                        store,
                        clean_name.clone(),
                        bytes,
                        mime_from_file_name(&clean_name),
                        clean_name,
                    ) {
                        Ok(media) => saved.push(media),
                        Err(error) => eprintln!("SynthChat wechat file save failed: {error}"),
                    }
                }
                Err(error) => eprintln!("SynthChat wechat file download failed: {error}"),
            }
        } else if item_type == WECHAT_VOICE_ITEM_TYPE {
            let Some(voice_item) = item.get("voice_item").or_else(|| item.get("voiceItem")) else {
                continue;
            };
            let encode_type = voice_item
                .get("encode_type")
                .or_else(|| voice_item.get("encodeType"))
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let sample_rate = voice_item
                .get("sample_rate")
                .or_else(|| voice_item.get("sampleRate"))
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let playtime = voice_item
                .get("playtime")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let text = voice_item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(text) = text {
                eprintln!(
                    "SynthChat wechat voice transcribed by upstream: account={} encode={} sample_rate={} playtime={}ms text={}",
                    account.id, encode_type, sample_rate, playtime, text
                );
                continue;
            }
            let Some(media) = voice_item.get("media") else {
                continue;
            };
            match download_wechat_cdn_media(&client, media, WECHAT_VOICE_MAX_BYTES).await {
                Ok(bytes) => {
                    let extension = wechat_voice_extension(encode_type);
                    let label = format!(
                        "语音 encode={encode_type} sample_rate={sample_rate} playtime={playtime}ms"
                    );
                    let name = format!("wechat-{}-{}.{}", account.id, new_id("voice"), extension);
                    match save_wechat_attachment(
                        store,
                        name,
                        bytes,
                        wechat_voice_mime_type(encode_type).to_string(),
                        label,
                    ) {
                        Ok(media) => saved.push(media),
                        Err(error) => eprintln!("SynthChat wechat voice save failed: {error}"),
                    }
                }
                Err(error) => eprintln!("SynthChat wechat voice download failed: {error}"),
            }
        }
    }
    saved
}

async fn extract_and_save_wechat_media_with_extras(
    store: &AppStore,
    account: &AccountConfig,
    raw_msg: Option<&Value>,
    attachments: &[Value],
) -> Vec<WechatInboundMedia> {
    let mut media = Vec::new();
    if let Some(raw_msg) = raw_msg {
        media.extend(extract_and_save_wechat_media(store, account, raw_msg).await);
        let mut raw_attachments = Vec::new();
        collect_extra_attachment_values(raw_msg, &mut raw_attachments, 0);
        media.extend(media_from_extra_attachments(store, &raw_attachments));
    }
    media.extend(media_from_extra_attachments(store, attachments));
    let mut seen = std::collections::HashSet::new();
    media
        .into_iter()
        .filter(|item| seen.insert(format!("{}::{}", item.path, item.mime_type)))
        .collect()
}

fn append_media_contexts(content: &str, media: &[WechatInboundMedia]) -> String {
    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content.trim().to_string());
    }
    for item in media {
        parts.push(format!(
            "[media attached: \"{}\" ({})] {}",
            item.path, item.mime_type, item.label
        ));
        parts.push(
            json!({
                "type": "attachment",
                "id": item.id,
                "path": item.path,
                "fileName": item.label,
                "mimeType": item.mime_type,
            })
            .to_string(),
        );
    }
    parts.join("\n\n")
}

pub fn get_wechat_config() -> AppResult<WechatConfig> {
    let mut config = read_json("wechat.json", default_wechat_config())?;
    if should_replace_with_default_base_url(&config.base_url) {
        let defaults = default_wechat_config();
        config.base_url = defaults.base_url;
        if config.timeout_seconds == 0 {
            config.timeout_seconds = defaults.timeout_seconds;
        }
        config.timeout_seconds = config.timeout_seconds.max(5);
        write_json("wechat.json", &config)?;
    }
    Ok(config)
}

pub fn save_wechat_config(mut config: WechatConfig) -> AppResult<WechatConfig> {
    config.base_url = if should_replace_with_default_base_url(&config.base_url) {
        default_wechat_config().base_url
    } else {
        normalize_base_url(&config.base_url)?
    };
    config.timeout_seconds = config.timeout_seconds.max(5);
    write_json("wechat.json", &config)?;
    Ok(config)
}

pub fn list_accounts() -> AppResult<Vec<AccountConfig>> {
    read_json("accounts.json", Vec::<AccountConfig>::new())
}

pub fn save_accounts(mut accounts: Vec<AccountConfig>) -> AppResult<()> {
    accounts.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    write_json("accounts.json", &accounts)
}

pub async fn start_wechat_qr(base_url: Option<String>) -> AppResult<WechatQrStartResult> {
    let config = get_wechat_config()?;
    let base_url = normalize_base_url(base_url.as_deref().unwrap_or(&config.base_url))?;
    let timeout = Duration::from_secs(config.timeout_seconds.clamp(5, 20));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .get(format!("{base_url}/ilink/bot/get_bot_qrcode"))
        .headers(common_ilink_headers())
        .query(&[("bot_type", "3")])
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat QR code", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat QR code endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat QR code response", error))?;

    let qrcode = first_value_string(
        &raw,
        &[
            &["qrcode"],
            &["data", "qrcode"],
            &["result", "qrcode"],
            &["qrCode"],
            &["data", "qrCode"],
        ],
    )
    .ok_or_else(|| AppError::BadRequest("wechat QR response does not include qrcode".into()))?;
    let qr_content = first_value_string(
        &raw,
        &[
            &["qrcode_img_content"],
            &["data", "qrcode_img_content"],
            &["qr_image"],
            &["data", "qr_image"],
            &["qrImage"],
            &["data", "qrImage"],
        ],
    )
    .filter(|value| !value.trim().is_empty())
    .unwrap_or_else(|| qrcode.clone());
    let qr_image = Some(qr_image_from_content(&qr_content)?);

    Ok(WechatQrStartResult {
        qrcode,
        qr_image,
        base_url,
        raw,
    })
}

pub async fn check_wechat_qr_status(
    qrcode: String,
    base_url: Option<String>,
) -> AppResult<WechatQrStatusResult> {
    let trimmed_qrcode = qrcode.trim();
    if trimmed_qrcode.is_empty() {
        return Err(AppError::BadRequest("qrcode is required".into()));
    }
    let config = get_wechat_config()?;
    let base_url = normalize_base_url(base_url.as_deref().unwrap_or(&config.base_url))?;
    let timeout = Duration::from_secs(
        config
            .timeout_seconds
            .clamp(WECHAT_QR_STATUS_TIMEOUT_SECONDS, 60),
    );
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .get(format!("{base_url}/ilink/bot/get_qrcode_status"))
        .headers(common_ilink_headers())
        .query(&[("qrcode", trimmed_qrcode)])
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat QR status", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat QR status endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat QR status response", error))?;

    let status = first_value_string(
        &raw,
        &[
            &["status"],
            &["data", "status"],
            &["result", "status"],
            &["state"],
            &["data", "state"],
        ],
    )
    .unwrap_or_else(|| "unknown".to_string());
    let normalized = status.to_ascii_lowercase();
    let message = first_value_string(
        &raw,
        &[
            &["message"],
            &["msg"],
            &["data", "message"],
            &["data", "msg"],
        ],
    );
    let host = first_value_string(
        &raw,
        &[
            &["host"],
            &["data", "host"],
            &["redirect_host"],
            &["data", "redirect_host"],
            &["base_url"],
            &["data", "base_url"],
            &["baseurl"],
            &["data", "baseurl"],
        ],
    );

    let confirmed = [
        "confirmed",
        "success",
        "ok",
        "logged_in",
        "login_success",
        "2",
        "3",
    ]
    .contains(&normalized.as_str());
    let account = if confirmed {
        let mut accounts = list_accounts()?;
        let resolved_ilink_user_id = first_value_string(
            &raw,
            &[
                &["ilink_user_id"],
                &["data", "ilink_user_id"],
                &["user_id"],
                &["data", "user_id"],
                &["data", "bot", "ilink_user_id"],
            ],
        )
        .unwrap_or_default();
        let account_id = first_value_string(
            &raw,
            &[
                &["ilink_bot_id"],
                &["data", "ilink_bot_id"],
                &["bot_id"],
                &["botId"],
                &["data", "bot_id"],
                &["data", "botId"],
                &["data", "bot", "bot_id"],
                &["data", "bot", "botId"],
            ],
        )
        .or_else(|| {
            if resolved_ilink_user_id.trim().is_empty() {
                None
            } else {
                Some(resolved_ilink_user_id.clone())
            }
        })
        .unwrap_or_else(|| format!("wechat-{}", new_id("account")));
        let existing_index = accounts.iter().position(|account| {
            account.id == account_id
                || (!resolved_ilink_user_id.trim().is_empty()
                    && account.ilink_user_id == resolved_ilink_user_id)
        });
        let existing = existing_index.and_then(|index| accounts.get(index));
        let now = now_iso();
        let resolved_bot_token = first_value_string(
            &raw,
            &[
                &["bot_token"],
                &["data", "bot_token"],
                &["ilink_bot_token"],
                &["data", "ilink_bot_token"],
                &["data", "bot", "bot_token"],
            ],
        )
        .or_else(|| existing.map(|account| account.bot_token.clone()))
        .unwrap_or_default();
        if resolved_bot_token.trim().is_empty() {
            return Err(AppError::BadRequest(
                "wechat QR confirmed but credential payload was incomplete: missing bot token"
                    .into(),
            ));
        }
        let saved = AccountConfig {
            id: account_id.clone(),
            note: first_value_string(
                &raw,
                &[
                    &["nickname"],
                    &["data", "nickname"],
                    &["data", "bot", "nickname"],
                    &["name"],
                    &["data", "name"],
                ],
            )
            .or_else(|| existing.map(|account| account.note.clone()))
            .unwrap_or_else(|| "微信账号".to_string()),
            linked_persona: existing
                .map(|account| account.linked_persona.clone())
                .unwrap_or_default(),
            online: true,
            created_at: existing
                .map(|account| account.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            bot_token: resolved_bot_token,
            ilink_user_id: if resolved_ilink_user_id.trim().is_empty() {
                existing
                    .map(|account| account.ilink_user_id.clone())
                    .unwrap_or_default()
            } else {
                resolved_ilink_user_id.clone()
            },
            get_updates_buf: first_value_string(
                &raw,
                &[
                    &["get_updates_buf"],
                    &["data", "get_updates_buf"],
                    &["getUpdatesBuf"],
                    &["data", "getUpdatesBuf"],
                ],
            )
            .or_else(|| existing.map(|account| account.get_updates_buf.clone()))
            .unwrap_or_default(),
            login_base_url: base_url.clone(),
            last_login_at: now_iso(),
            last_wechat_user_id: existing
                .map(|account| account.last_wechat_user_id.clone())
                .unwrap_or_default(),
            last_context_token: existing
                .map(|account| account.last_context_token.clone())
                .unwrap_or_default(),
            last_inbound_at: existing
                .map(|account| account.last_inbound_at.clone())
                .unwrap_or_default(),
            raw_login_status: Some(raw.clone()),
        };
        if let Some(index) = existing_index {
            accounts[index] = saved.clone();
        } else {
            accounts.push(saved.clone());
        }
        save_accounts(accounts)?;
        Some(saved)
    } else {
        None
    };

    Ok(WechatQrStatusResult {
        status,
        message,
        account,
        host,
        raw,
    })
}

pub fn list_wechat_links(personas: Vec<Persona>) -> AppResult<Vec<WechatLinkSummary>> {
    Ok(list_accounts()?
        .into_iter()
        .filter(|account| !account.linked_persona.trim().is_empty())
        .map(|account| {
            let persona = personas
                .iter()
                .find(|persona| persona.id == account.linked_persona);
            WechatLinkSummary {
                account_id: account.id,
                persona_id: account.linked_persona,
                persona_name: persona
                    .map(|persona| persona.name.clone())
                    .unwrap_or_else(|| "未知角色".to_string()),
                account_note: account.note,
                online: account.online,
            }
        })
        .collect())
}

pub fn link_wechat_account(
    persona_id: String,
    account_id: String,
) -> AppResult<Vec<AccountConfig>> {
    let persona_id = persona_id.trim().to_string();
    let account_id = account_id.trim().to_string();
    if persona_id.is_empty() || account_id.is_empty() {
        return Err(AppError::BadRequest(
            "personaId and accountId are required".into(),
        ));
    }
    let mut accounts = list_accounts()?;
    for account in &mut accounts {
        if account.linked_persona == persona_id || account.id == account_id {
            account.linked_persona.clear();
        }
        if account.id == account_id {
            account.linked_persona = persona_id.clone();
        }
    }
    save_accounts(accounts.clone())?;
    Ok(accounts)
}

pub fn unlink_wechat_account(persona_id: String) -> AppResult<Vec<AccountConfig>> {
    let persona_id = persona_id.trim();
    let mut accounts = list_accounts()?;
    for account in &mut accounts {
        if account.linked_persona == persona_id {
            account.linked_persona.clear();
        }
    }
    save_accounts(accounts.clone())?;
    Ok(accounts)
}

pub fn remember_wechat_reply_context(
    account_id: &str,
    user_id: &str,
    context_token: Option<&str>,
) -> AppResult<()> {
    let clean_user_id = user_id.trim();
    if clean_user_id.is_empty() {
        return Ok(());
    }
    let mut accounts = list_accounts()?;
    if let Some(account) = accounts.iter_mut().find(|account| account.id == account_id) {
        account.last_wechat_user_id = clean_user_id.to_string();
        if let Some(token) = context_token
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            account.last_context_token = token.to_string();
        }
        account.last_inbound_at = now_iso();
        save_accounts(accounts)?;
    }
    Ok(())
}

pub fn extract_wechat_text(raw_msg: &Value) -> Option<String> {
    if let Some(text) = first_value_string(
        raw_msg,
        &[
            &["text"],
            &["content"],
            &["message"],
            &["msg"],
            &["text_item", "text"],
            &["textItem", "text"],
        ],
    )
    .filter(|value| !value.trim().is_empty())
    {
        return Some(text);
    }
    raw_msg
        .get("item_list")
        .or_else(|| raw_msg.get("itemList"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                first_value_string(
                    item,
                    &[
                        &["text_item", "text"],
                        &["textItem", "text"],
                        &["text"],
                        &["content"],
                    ],
                )
                .filter(|value| !value.trim().is_empty())
                .or_else(|| {
                    first_value_string(item, &[&["voice_item", "text"], &["voiceItem", "text"]])
                        .filter(|value| !value.trim().is_empty())
                })
            })
        })
        .or_else(|| recursive_value_string(raw_msg, &["text", "content", "message"]))
}

pub fn extract_wechat_user_id(raw_msg: &Value) -> Option<String> {
    first_value_string(
        raw_msg,
        &[
            &["from_user_id"],
            &["fromUserId"],
            &["user_id"],
            &["userId"],
            &["sender"],
            &["from", "user_id"],
            &["from", "userId"],
        ],
    )
    .filter(|value| !value.trim().is_empty())
    .or_else(|| {
        recursive_value_string(
            raw_msg,
            &["from_user_id", "fromUserId", "user_id", "userId", "sender"],
        )
    })
}

pub fn extract_wechat_context_token(raw_msg: &Value) -> Option<String> {
    first_value_string(
        raw_msg,
        &[
            &["context_token"],
            &["contextToken"],
            &["chat_context", "context_token"],
            &["chatContext", "contextToken"],
        ],
    )
    .filter(|value| !value.trim().is_empty())
    .or_else(|| recursive_value_string(raw_msg, &["context_token", "contextToken"]))
}

fn find_or_create_wechat_conversation(
    store: &AppStore,
    persona_id: &str,
    account_id: &str,
) -> AppResult<String> {
    let conversations = store.conversations()?;
    if let Some(existing) = conversations.iter().find(|conversation| {
        conversation.wechat_account_id.as_deref() == Some(account_id)
            || conversation
                .metadata
                .get("wechatAccountId")
                .and_then(Value::as_str)
                == Some(account_id)
    }) {
        if let Some(persona_conversation) = conversations.iter().find(|conversation| {
            conversation.persona_id.as_deref() == Some(persona_id)
                && conversation.id != existing.id
                && conversation.wechat_account_id.is_none()
                && conversation
                    .metadata
                    .get("wechatAccountId")
                    .and_then(Value::as_str)
                    .is_none()
        }) {
            let merged = store.merge_conversation_into(&existing.id, &persona_conversation.id)?;
            let _ = store.set_conversation_wechat_account(&merged.id, Some(account_id.to_string()));
            let _ = store.set_conversation_metadata_value(&merged.id, "platform", json!("wechat"));
            let _ = store.set_conversation_metadata_value(
                &merged.id,
                "wechatAccountId",
                json!(account_id),
            );
            return Ok(merged.id);
        }
        if existing.wechat_account_id.as_deref() != Some(account_id) {
            let _ =
                store.set_conversation_wechat_account(&existing.id, Some(account_id.to_string()));
        }
        return Ok(existing.id.clone());
    }
    if let Some(existing) = conversations
        .iter()
        .find(|conversation| conversation.persona_id.as_deref() == Some(persona_id))
    {
        let _ = store.set_conversation_wechat_account(&existing.id, Some(account_id.to_string()));
        let _ = store.set_conversation_metadata_value(&existing.id, "platform", json!("wechat"));
        let _ = store.set_conversation_metadata_value(
            &existing.id,
            "wechatAccountId",
            json!(account_id),
        );
        return Ok(existing.id.clone());
    }
    let conversation = store.create_conversation(None, Some(persona_id.to_string()))?;
    let _ = store.set_conversation_wechat_account(&conversation.id, Some(account_id.to_string()));
    let _ = store.set_conversation_metadata_value(&conversation.id, "platform", json!("wechat"));
    let _ = store.set_conversation_metadata_value(
        &conversation.id,
        "wechatAccountId",
        json!(account_id),
    );
    Ok(conversation.id)
}

fn wechat_chat_thread_stack_size() -> usize {
    std::env::var("SYNTHCHAT_WECHAT_CHAT_STACK_MB")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_WECHAT_CHAT_THREAD_STACK_SIZE)
        .clamp(
            MIN_WECHAT_CHAT_THREAD_STACK_SIZE,
            MAX_WECHAT_CHAT_THREAD_STACK_SIZE,
        )
}

async fn run_wechat_chat_turn(
    store: &AppStore,
    request: SendChatRequest,
    app: &AppHandle,
) -> AppResult<Vec<ChatMessage>> {
    let store = store.clone();
    let app = app.clone();
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("synthchat-wechat-chat-turn".to_string())
        .stack_size(wechat_chat_thread_stack_size())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(agent::run_chat_turn(&store, request, Some(&app))),
                Err(error) => Err(AppError::BadRequest(format!(
                    "failed to create wechat chat runtime: {error}"
                ))),
            };
            let _ = tx.send(result);
        })
        .map_err(|error| {
            AppError::BadRequest(format!("failed to spawn wechat chat thread: {error}"))
        })?;
    rx.recv()
        .map_err(|error| AppError::BadRequest(format!("wechat chat thread failed: {error}")))?
}

async fn dispatch_reply_to_wechat(
    account: &AccountConfig,
    user_id: &str,
    reply: &str,
    context_token: Option<&str>,
) -> (bool, Option<String>) {
    let image_paths = extract_wechat_image_paths(reply);
    let voice_paths = extract_wechat_voice_paths(reply);
    let file_paths = extract_wechat_file_paths(reply);
    let mobile_text = strip_wechat_media_marker_lines(reply);
    if mobile_text.trim().is_empty()
        && image_paths.is_empty()
        && voice_paths.is_empty()
        && file_paths.is_empty()
    {
        return (true, None);
    }
    let mut errors = Vec::new();
    for image_path in &image_paths {
        if let Err(error) =
            send_wechat_image_message_with_retry(account, user_id, image_path, context_token).await
        {
            errors.push(format!("图片发送失败 {image_path}: {error}"));
        }
    }
    for voice_path in &voice_paths {
        if let Err(error) =
            send_wechat_voice_message_with_retry(account, user_id, voice_path, context_token).await
        {
            errors.push(format!("语音发送失败 {voice_path}: {error}"));
        }
    }
    for file_path in &file_paths {
        if let Err(error) =
            send_wechat_file_message_with_retry(account, user_id, file_path, context_token).await
        {
            errors.push(format!("文件发送失败 {file_path}: {error}"));
        }
    }
    if !mobile_text.trim().is_empty() {
        let text_token =
            if image_paths.is_empty() && voice_paths.is_empty() && file_paths.is_empty() {
                context_token
            } else {
                None
            };
        if let Err(error) =
            send_wechat_text_message_with_retry(account, user_id, &mobile_text, text_token).await
        {
            errors.push(format!("文本发送失败: {error}"));
        }
    }
    if errors.is_empty() {
        (true, None)
    } else {
        (false, Some(errors.join("\n")))
    }
}

pub async fn wechat_poll_once(
    store: &AppStore,
    app: &AppHandle,
    account_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<WechatPollResult> {
    let account_id = account_id.trim().to_string();
    if account_id.is_empty() {
        return Err(AppError::BadRequest("accountId is required".into()));
    }
    let mut accounts = list_accounts()?;
    let account_index = accounts
        .iter()
        .position(|account| account.id == account_id)
        .ok_or_else(|| AppError::NotFound(format!("wechat account not found: {account_id}")))?;
    let account = accounts[account_index].clone();
    let base_url = normalize_base_url(if account.login_base_url.trim().is_empty() {
        DEFAULT_WECHAT_BASE_URL
    } else {
        account.login_base_url.trim()
    })?;
    let poll_timeout = timeout_seconds.unwrap_or(35).clamp(5, 120);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(poll_timeout + 5))
        .build()
        .map_err(|error| wechat_http_error("failed to create wechat HTTP client", error))?;
    let raw: Value = client
        .post(format!("{base_url}/ilink/bot/getupdates"))
        .headers(ilink_headers_with_token(&account.bot_token)?)
        .json(&json!({
            "get_updates_buf": account.get_updates_buf,
            "base_info": wechat_base_info()
        }))
        .send()
        .await
        .map_err(|error| wechat_http_error("failed to request wechat updates", error))?
        .error_for_status()
        .map_err(|error| wechat_http_error("wechat getupdates endpoint returned an error", error))?
        .json()
        .await
        .map_err(|error| wechat_http_error("failed to read wechat updates response", error))?;

    let errcode = raw.get("errcode").and_then(Value::as_i64).unwrap_or(0);
    if errcode != 0 {
        let errmsg =
            first_value_string(&raw, &[&["errmsg"], &["message"], &["msg"]]).unwrap_or_default();
        if errcode == -14 {
            accounts[account_index].online = false;
            save_accounts(accounts)?;
        }
        return Err(AppError::BadRequest(format!(
            "getUpdates errcode={errcode}: {errmsg}"
        )));
    }

    let next_updates_buf = first_value_string(&raw, &[&["get_updates_buf"], &["getUpdatesBuf"]]);

    let msgs = raw
        .get("msgs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let received_count = msgs.len();
    if received_count > 0 {
        eprintln!(
            "SynthChat wechat poll received: account={} count={}",
            account_id, received_count
        );
    }
    let mut skipped_count = 0usize;
    let mut processed = Vec::new();
    for raw_msg in msgs {
        let Some(user_id) = extract_wechat_user_id(&raw_msg) else {
            eprintln!(
                "SynthChat wechat poll skipped message without user id: {}",
                raw_msg
            );
            skipped_count += 1;
            continue;
        };
        let text = extract_wechat_text(&raw_msg).unwrap_or_default();
        if text.trim().is_empty() && !raw_wechat_message_has_media(&raw_msg) {
            eprintln!(
                "SynthChat wechat poll skipped empty message: account={} user={}",
                account_id, user_id
            );
            skipped_count += 1;
            continue;
        }
        let context_token = extract_wechat_context_token(&raw_msg);
        let result = wechat_inbound_text_with_extras(
            store,
            app,
            account_id.clone(),
            user_id.clone(),
            text,
            context_token,
            WechatInboundExtras {
                raw_message: Some(raw_msg),
                attachments: Vec::new(),
            },
        )
        .await?;
        eprintln!(
            "SynthChat wechat inbound processed: account={} user={} delivered={}",
            account_id, user_id, result.delivered
        );
        processed.push(WechatProcessedInbound {
            user_id,
            text: result
                .messages
                .iter()
                .filter_map(|message| {
                    (message.get("role").and_then(Value::as_str) == Some("user"))
                        .then(|| message.get("content").and_then(Value::as_str))
                        .flatten()
                })
                .last()
                .unwrap_or_default()
                .to_string(),
            conversation_id: result
                .messages
                .iter()
                .filter_map(|message| {
                    message
                        .get("conversationId")
                        .or_else(|| message.get("conversation_id"))
                        .and_then(Value::as_str)
                })
                .next()
                .map(str::to_string),
            delivered: result.delivered,
            delivery_error: result.delivery_error,
        });
    }

    let mut updated_buffer = false;
    if let Some(new_buf) = next_updates_buf {
        let mut accounts = list_accounts()?;
        if let Some(account) = accounts.iter_mut().find(|account| account.id == account_id) {
            account.get_updates_buf = new_buf;
            save_accounts(accounts)?;
            updated_buffer = true;
        }
    }

    let account = list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| AppError::NotFound(format!("wechat account not found: {account_id}")))?;
    Ok(WechatPollResult {
        account,
        processed,
        received_count,
        skipped_count,
        updated_buffer,
        raw,
    })
}

fn env_u64(name: &str, fallback: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(fallback)
        .clamp(min, max)
}

pub async fn run_wechat_poll_loop(store: AppStore, app: AppHandle) {
    let interval_seconds = env_u64("SYNTHCHAT_WECHAT_POLL_INTERVAL_SECONDS", 2, 1, 60);
    let timeout_seconds = env_u64("SYNTHCHAT_WECHAT_POLL_TIMEOUT_SECONDS", 25, 5, 120);
    loop {
        let accounts = list_accounts().unwrap_or_default();
        let targets = accounts
            .into_iter()
            .filter(|account| {
                account.online
                    && !account.bot_token.trim().is_empty()
                    && !account.linked_persona.trim().is_empty()
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            eprintln!("SynthChat wechat poll idle: no online linked accounts");
        }
        for account in targets {
            eprintln!(
                "SynthChat wechat poll target: account={} persona={}",
                account.id, account.linked_persona
            );
            let result = wechat_poll_once(
                &store,
                &app,
                account.id.clone(),
                Some(timeout_seconds),
            )
            .await;
            if let Err(error) = result {
                let _ = app.emit(
                    "synthchat-wechat-poll-error",
                    json!({
                        "accountId": account.id,
                        "error": error.to_string(),
                    }),
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(interval_seconds)).await;
    }
}

pub async fn wechat_inbound_text(
    store: &AppStore,
    app: &AppHandle,
    account_id: String,
    user_id: String,
    text: String,
    context_token: Option<String>,
) -> AppResult<WechatInboundResult> {
    wechat_inbound_text_with_extras(
        store,
        app,
        account_id,
        user_id,
        text,
        context_token,
        WechatInboundExtras::default(),
    )
    .await
}

pub async fn wechat_inbound_text_with_extras(
    store: &AppStore,
    app: &AppHandle,
    account_id: String,
    user_id: String,
    text: String,
    context_token: Option<String>,
    extras: WechatInboundExtras,
) -> AppResult<WechatInboundResult> {
    let content = if text.trim().is_empty() {
        extras
            .raw_message
            .as_ref()
            .and_then(extract_wechat_text)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        text.trim().to_string()
    };
    let accounts = list_accounts()?;
    let account = accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("wechat account not found: {account_id}")))?;
    if account.linked_persona.trim().is_empty() {
        return Err(AppError::BadRequest(
            "wechat account is not linked to a persona".into(),
        ));
    }
    remember_wechat_reply_context(&account.id, &user_id, context_token.as_deref())?;
    let persona = store.persona(Some(account.linked_persona.as_str()))?;
    let conversation_id =
        find_or_create_wechat_conversation(store, &account.linked_persona, &account.id)?;
    let media = extract_and_save_wechat_media_with_extras(
        store,
        &account,
        extras.raw_message.as_ref(),
        &extras.attachments,
    )
    .await;
    let content = append_media_contexts(&content, &media);
    if content.trim().is_empty() {
        return Err(AppError::BadRequest("message text or media is required".into()));
    }
    emit_wechat_processing(app, &conversation_id, &persona.id, true);
    let turn_started_at = now_iso();
    let messages_result = run_wechat_chat_turn(
        store,
        SendChatRequest {
            conversation_id: Some(conversation_id.clone()),
            persona_id: Some(persona.id.clone()),
            agent_id: Some(persona.agent_id.clone()),
            content,
            provider_data: Some(json!({
                "source": "wechat",
                "accountId": account.id,
                "userId": user_id,
                "contextToken": context_token.clone(),
                "agentId": persona.agent_id,
            })),
            queue_item_id: None,
        },
        app,
    )
    .await;
    let mut messages = match messages_result {
        Ok(messages) => messages,
        Err(error) => {
            emit_wechat_processing(app, &conversation_id, &persona.id, false);
            return Err(error);
        }
    };
    let user_message = messages
        .iter()
        .rev()
        .find(|message| message.role == "user" && message.source == "wechat")
        .cloned();
    if let Some(message) = user_message.as_ref() {
        // Re-emit from the caller thread so the desktop window cannot miss the
        // persisted WeChat user message due to cross-thread event timing.
        emit_wechat_user_message(app, &conversation_id, &persona.id, message);
    }
    emit_wechat_processing(app, &conversation_id, &persona.id, false);
    let mut reply_message = messages
        .iter()
        .rev()
        .find(|message| is_wechat_deliverable_assistant_message(message))
        .cloned();
    if let Some(message) = reply_message.as_ref() {
        if let Some(attached) =
            attach_wechat_deliverable_to_reply(store, &conversation_id, &turn_started_at, message)?
        {
            let attached_id = attached.id.clone();
            reply_message = Some(attached.clone());
            for item in &mut messages {
                if item.id == attached_id {
                    *item = attached.clone();
                }
            }
        }
    }
    if let Some(message) = reply_message.as_ref() {
        persist_wechat_assistant_message_if_missing(store, message)?;
        emit_wechat_assistant_message(app, &conversation_id, &persona.id, message);
    }
    let reply = reply_message
        .as_ref()
        .map(|message| message.content.clone())
        .unwrap_or_default();
    let (delivered, delivery_error) =
        dispatch_reply_to_wechat(&account, &user_id, &reply, context_token.as_deref()).await;
    Ok(WechatInboundResult {
        messages: messages
            .into_iter()
            .map(|message: ChatMessage| serde_json::to_value(message).unwrap_or_else(|_| json!({})))
            .collect(),
        delivered,
        delivery_error,
    })
}

fn attach_wechat_deliverable_to_reply(
    store: &AppStore,
    conversation_id: &str,
    turn_started_at: &str,
    message: &ChatMessage,
) -> AppResult<Option<ChatMessage>> {
    if message.content.contains("[media attached:")
        || message.content.lines().any(|line| {
            line.trim()
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("MEDIA:"))
        })
    {
        return Ok(None);
    }
    store.attach_wechat_deliverable_to_message_after(
        conversation_id,
        &message.id,
        turn_started_at,
        Some("附件已补到回复。"),
    )
}

fn persist_wechat_assistant_message_if_missing(
    store: &AppStore,
    message: &ChatMessage,
) -> AppResult<()> {
    let exists = store
        .messages(&message.conversation_id, None)?
        .iter()
        .any(|candidate| candidate.id == message.id);
    if !exists {
        let _ = store.append_message(message.clone())?;
    }
    Ok(())
}

fn emit_wechat_assistant_message(
    app: &AppHandle,
    conversation_id: &str,
    persona_id: &str,
    message: &ChatMessage,
) {
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "assistant_message",
            "source": "wechat",
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": message,
            "isLast": true,
        }),
    );
    let _ = app.emit(
        "synthchat-pet-event",
        json!({
            "type": "assistant_final",
            "source": "wechat",
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": message,
        }),
    );
}

fn emit_wechat_user_message(
    app: &AppHandle,
    conversation_id: &str,
    persona_id: &str,
    message: &ChatMessage,
) {
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "new_message",
            "source": "wechat",
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": message,
            "isLast": false,
        }),
    );
}

fn emit_wechat_processing(
    app: &AppHandle,
    conversation_id: &str,
    persona_id: &str,
    processing: bool,
) {
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": if processing { "processing" } else { "conversation_updated" },
            "source": "wechat",
            "personaId": persona_id,
            "conversationId": conversation_id,
        }),
    );
}

fn is_wechat_deliverable_assistant_message(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && message.source != "desktop-agent-error"
        && message.source != "desktop-control"
        && message.source != "desktop-diagnosis"
        && !message.source.starts_with("desktop-local-")
}

fn provider_data_string(provider_data: Option<&Value>, keys: &[&str]) -> Option<String> {
    let data = provider_data?;
    keys.iter().find_map(|key| {
        data.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

pub async fn finalize_queued_wechat_turn(
    store: &AppStore,
    messages: &[ChatMessage],
    provider_data: Option<&Value>,
    turn_started_at: Option<&str>,
) -> AppResult<()> {
    let source = provider_data_string(provider_data, &["source"]).unwrap_or_default();
    if source != "wechat" {
        return Ok(());
    }
    let account_id = match provider_data_string(provider_data, &["accountId", "account_id"]) {
        Some(value) => value,
        None => return Ok(()),
    };
    let user_id = match provider_data_string(provider_data, &["userId", "user_id"]) {
        Some(value) => value,
        None => return Ok(()),
    };
    let Some(account) = list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
    else {
        return Ok(());
    };
    let mut reply_message = messages
        .iter()
        .rev()
        .find(|message| is_wechat_deliverable_assistant_message(message))
        .cloned();
    if let Some(message) = reply_message.as_ref() {
        if let Some(started_at) = turn_started_at {
            if let Some(attached) = attach_wechat_deliverable_to_reply(
                store,
                &message.conversation_id,
                started_at,
                message,
            )? {
                reply_message = Some(attached);
            }
        }
    }
    let reply = reply_message
        .as_ref()
        .map(|message| message.content.clone())
        .unwrap_or_default();
    if reply.trim().is_empty() {
        return Ok(());
    }
    let context_token = provider_data_string(provider_data, &["contextToken", "context_token"]);
    let (_, delivery_error) =
        dispatch_reply_to_wechat(&account, &user_id, &reply, context_token.as_deref()).await;
    if let Some(error) = delivery_error {
        eprintln!(
            "SynthChat queued wechat delivery failed: account={} user={} error={}",
            account.id, user_id, error
        );
    }
    Ok(())
}

pub fn dispatch_desktop_reply_to_wechat(conversation: &crate::models::Conversation, text: &str) {
    let reply = text.trim().to_string();
    let image_paths = extract_wechat_image_paths(&reply);
    let voice_paths = extract_wechat_voice_paths(&reply);
    let file_paths = extract_wechat_file_paths(&reply);
    let mobile_text = strip_wechat_media_marker_lines(&reply);
    if mobile_text.trim().is_empty()
        && image_paths.is_empty()
        && voice_paths.is_empty()
        && file_paths.is_empty()
    {
        return;
    }
    let account_id = conversation
        .wechat_account_id
        .as_deref()
        .map(str::to_string)
        .or_else(|| {
            conversation
                .metadata
                .get("wechatAccountId")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(account_id) = account_id else {
        return;
    };
    let Ok(accounts) = list_accounts() else {
        return;
    };
    let account = accounts
        .into_iter()
        .find(|account| account.id == account_id);
    let Some(account) = account else {
        return;
    };
    let user_id = account.last_wechat_user_id.trim().to_string();
    if user_id.is_empty() {
        return;
    }
    let context_token = account.last_context_token.trim().to_string();
    tauri::async_runtime::spawn(async move {
        let token = if context_token.trim().is_empty() {
            None
        } else {
            Some(context_token.as_str())
        };
        let mut errors = Vec::new();
        for image_path in &image_paths {
            if let Err(error) =
                send_wechat_image_message_with_retry(&account, &user_id, image_path, token).await
            {
                errors.push(format!("图片发送失败 {image_path}: {error}"));
            }
        }
        for voice_path in &voice_paths {
            if let Err(error) =
                send_wechat_voice_message_with_retry(&account, &user_id, voice_path, token).await
            {
                errors.push(format!("语音发送失败 {voice_path}: {error}"));
            }
        }
        for file_path in &file_paths {
            if let Err(error) =
                send_wechat_file_message_with_retry(&account, &user_id, file_path, token).await
            {
                errors.push(format!("文件发送失败 {file_path}: {error}"));
            }
        }
        if !mobile_text.trim().is_empty() {
            let text_token =
                if image_paths.is_empty() && voice_paths.is_empty() && file_paths.is_empty() {
                    token
                } else {
                    None
                };
            if let Err(error) =
                send_wechat_text_message_with_retry(&account, &user_id, &mobile_text, text_token)
                    .await
            {
                errors.push(format!("文本发送失败: {error}"));
            }
        }
        if !errors.is_empty() {
            eprintln!(
                "SynthChat wechat desktop delivery failed: {}",
                errors.join("\n")
            );
        }
    });
}

#[allow(dead_code)]
pub fn _keep_app_handle_type(_: Option<AppHandle>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wechat_aes_roundtrip_and_key_parser_accepts_hex_b64() {
        let key = *b"0123456789abcdef";
        let encrypted = aes_128_ecb_pkcs7_encrypt(b"wechat media payload", &key);
        assert_ne!(encrypted, b"wechat media payload");
        let decrypted = aes_128_ecb_pkcs7_decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, b"wechat media payload");

        let encoded_hex = general_purpose::STANDARD.encode(bytes_to_lower_hex(&key).as_bytes());
        assert_eq!(parse_wechat_aes_key(&encoded_hex).unwrap(), key);
        let encoded_raw = general_purpose::STANDARD.encode(key);
        assert_eq!(parse_wechat_aes_key(&encoded_raw).unwrap(), key);
    }

    #[test]
    fn wechat_media_marker_extracts_and_classifies_paths() {
        let dir = std::env::temp_dir().join(format!("synthchat-wechat-test-{}", new_id("case")));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("a.png");
        let voice = dir.join("b.mp3");
        let file = dir.join("c.pdf");
        let media_tag_file = dir.join("d.docx");
        fs::write(&image, b"png").unwrap();
        fs::write(&voice, b"mp3").unwrap();
        fs::write(&file, b"pdf").unwrap();
        fs::write(&media_tag_file, b"docx").unwrap();
        let text = format!(
            "hello\n[media attached: {} (image/png)]\n[media attached: \"{}\" (audio/mpeg)]\n[media attached: `{}` (application/pdf)]\nMEDIA:\"{}\"",
            image.display(),
            voice.display(),
            file.display(),
            media_tag_file.display()
        );
        assert_eq!(
            extract_wechat_image_paths(&text),
            vec![image.to_string_lossy().to_string()]
        );
        assert_eq!(
            extract_wechat_voice_paths(&text),
            vec![voice.to_string_lossy().to_string()]
        );
        assert_eq!(
            extract_wechat_file_paths(&text),
            vec![
                file.to_string_lossy().to_string(),
                media_tag_file.to_string_lossy().to_string()
            ]
        );
        assert_eq!(strip_wechat_media_marker_lines(&text), "hello");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn extract_wechat_text_accepts_nested_item_list_text() {
        let raw = json!({
            "from": {
                "user_id": "wechat-user"
            },
            "chat_context": {
                "context_token": "ctx-1"
            },
            "item_list": [
                {
                    "text_item": {
                        "text": "你好"
                    }
                }
            ]
        });
        assert_eq!(extract_wechat_user_id(&raw).as_deref(), Some("wechat-user"));
        assert_eq!(extract_wechat_text(&raw).as_deref(), Some("你好"));
        assert_eq!(extract_wechat_context_token(&raw).as_deref(), Some("ctx-1"));
    }

    #[test]
    fn append_media_contexts_keeps_transcribed_voice_text_plain() {
        let content = append_media_contexts("这是一条语音转文字", &[]);
        assert_eq!(content, "这是一条语音转文字");
        assert!(!content.contains("media attached"));
    }

    #[test]
    fn wechat_delivery_filter_skips_control_assistant_messages() {
        let control = ChatMessage::new(
            "conv-1".into(),
            "assistant",
            "queued".into(),
            "desktop-control",
        );
        let normal = ChatMessage::new(
            "conv-1".into(),
            "assistant",
            "正常回复".into(),
            "desktop-agent",
        );
        assert!(!is_wechat_deliverable_assistant_message(&control));
        assert!(is_wechat_deliverable_assistant_message(&normal));
    }

    #[test]
    fn append_media_contexts_adds_attachment_json_for_agent() {
        let media = vec![WechatInboundMedia {
            id: "wechat_attachment_1".to_string(),
            path: "D:\\tmp\\report.txt".to_string(),
            mime_type: "text/plain".to_string(),
            label: "report.txt".to_string(),
        }];
        let content = append_media_contexts("请看文件", &media);
        assert!(content.contains("[media attached: \"D:\\tmp\\report.txt\" (text/plain)]"));
        assert!(content.contains("\"type\":\"attachment\""));
        assert!(content.contains("\"fileName\":\"report.txt\""));
    }

    #[test]
    fn extra_attachment_data_url_enters_wechat_media_contexts() {
        let dir = std::env::temp_dir().join(format!("synthchat-wechat-extra-{}", new_id("case")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let attachment = json!({
            "id": "att-image",
            "fileName": "photo.png",
            "mimeType": "image/png",
            "dataUrl": "data:image/png;base64,iVBORw0KGgo="
        });
        let media = media_from_extra_attachments(&store, &[attachment]);
        assert_eq!(media.len(), 1);
        let content = append_media_contexts("看图", &media);
        assert!(content.contains("[media attached:"));
        assert!(content.contains("image/png"));
        assert!(content.contains("\"type\":\"attachment\""));
        assert!(content.contains("\"fileName\":\"photo.png\""));
        assert!(PathBuf::from(&media[0].path).is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn extra_attachment_local_path_is_copied_into_attachment_cache() {
        let dir = std::env::temp_dir().join(format!("synthchat-wechat-local-extra-{}", new_id("case")));
        let external = dir.join("incoming");
        fs::create_dir_all(&external).unwrap();
        let source = external.join("photo");
        fs::write(&source, b"\x89PNG\r\n\x1a\nimage").unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let media = media_from_extra_attachments(
            &store,
            &[json!({
                "id": "local-image",
                "fileName": "photo",
                "path": source.to_string_lossy(),
            })],
        );
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].mime_type, "image/png");
        let saved_path = PathBuf::from(&media[0].path);
        assert!(saved_path.is_file());
        assert!(saved_path.starts_with(store.data_dir().join("attachments")));
        assert_ne!(saved_path, source);
        assert!(saved_path.extension().and_then(|value| value.to_str()).is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn find_or_create_wechat_conversation_reuses_persona_conversation() {
        let dir = std::env::temp_dir().join(format!("synthchat-wechat-store-{}", new_id("case")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let desktop = store
            .create_conversation(Some("desktop chat".into()), Some("default".into()))
            .unwrap();

        let resolved =
            find_or_create_wechat_conversation(&store, "default", "wechat-account").unwrap();
        assert_eq!(resolved, desktop.id);
        let conversation = store.conversation(&desktop.id).unwrap();
        assert_eq!(
            conversation.wechat_account_id.as_deref(),
            Some("wechat-account")
        );
        assert_eq!(
            conversation
                .metadata
                .get("wechatAccountId")
                .and_then(Value::as_str),
            Some("wechat-account")
        );
        assert_eq!(store.conversations().unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
