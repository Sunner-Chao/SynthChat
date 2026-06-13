use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{ChatMessage, Conversation},
    store::AppStore,
};

use super::{
    bluebubbles_configured, bluebubbles_send_message_tool, dingtalk_configured,
    dingtalk_send_message_tool, discord_settings, discord_tool, email_configured,
    email_send_message_tool, feishu_send_message_tool, feishu_settings, google_chat_configured,
    google_chat_send_message_tool, homeassistant_configured, homeassistant_send_message_tool,
    irc_configured, irc_send_message_tool, line_configured, line_postback_cache_set_ready,
    line_postback_pending_request_for_conversation, line_send_message_tool, matrix_configured,
    matrix_send_message_tool, mattermost_channel_directory, mattermost_configured,
    mattermost_send_message_tool, messaging_gateway_configured, messaging_gateway_platform_enabled,
    messaging_gateway_send_message_tool, ntfy_configured, ntfy_send_message_tool, qqbot_configured,
    qqbot_send_message_tool, signal_configured, signal_send_message_tool, simplex_configured,
    simplex_send_message_tool, slack_configured, slack_send_message_tool, sms_configured,
    sms_send_message_tool, string_arg, teams_configured, teams_send_message_tool,
    telegram_configured, telegram_send_message_tool, truncate_for_prompt, whatsapp_configured,
    whatsapp_send_message_tool, yuanbao_bridge_available, yuanbao_tool,
};
pub(super) fn send_message_tool(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = send_message_action(payload);
    match action.as_str() {
        "list" | "targets" => send_message_list_targets(store),
        "import_directory"
        | "import-directory"
        | "write_directory"
        | "write-directory"
        | "import_channel_directory"
        | "import-channel-directory" => send_message_import_channel_directory(payload),
        "refresh_directory"
        | "refresh-directory"
        | "refresh_channel_directory"
        | "refresh-channel-directory" => Err(AppError::BadRequest(
            "send_message refresh_directory requires async tool dispatch".into(),
        )),
        "send" | "create" | "post" | "" => {
            send_message_to_local_conversation(store, current_conversation_id, payload)
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported send_message action '{other}'. Use list or send."
        ))),
    }
}

pub(super) async fn send_message_tool_async(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    match send_message_action(payload).as_str() {
        "refresh_directory"
        | "refresh-directory"
        | "refresh_channel_directory"
        | "refresh-channel-directory" => {
            return send_message_refresh_channel_directory(store, payload).await;
        }
        "import_directory"
        | "import-directory"
        | "write_directory"
        | "write-directory"
        | "import_channel_directory"
        | "import-channel-directory" => {
            return send_message_import_channel_directory(payload);
        }
        _ => {}
    }
    if send_message_targets_discord(payload) {
        let discord_payloads = discord_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("discord", discord_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("discord", discord_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_discord_chunks(store, discord_payloads).await;
    }
    if send_message_targets_feishu(payload) {
        let feishu_payloads = feishu_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("feishu", feishu_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("feishu", feishu_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_feishu_chunks(store, feishu_payloads).await;
    }
    if send_message_targets_messaging_gateway(store, payload)? {
        let gateway_payloads = messaging_gateway_send_message_payloads(payload)?;
        let platform = gateway_payloads
            .first()
            .and_then(|payload| payload.get("platform"))
            .and_then(Value::as_str)
            .unwrap_or("messaging_gateway");
        if let Some(skipped) =
            send_message_silence_narration_skip(platform, gateway_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip(platform, gateway_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_messaging_gateway_chunks(store, gateway_payloads).await;
    }
    if send_message_targets_yuanbao(payload) {
        let yuanbao_payloads = yuanbao_send_message_payloads(payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("yuanbao", yuanbao_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("yuanbao", yuanbao_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_yuanbao_chunks(store, yuanbao_payloads).await;
    }
    if send_message_targets_telegram(payload) {
        let telegram_payloads = telegram_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("telegram", telegram_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("telegram", telegram_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_telegram_chunks(store, telegram_payloads).await;
    }
    if send_message_targets_slack(payload) {
        let slack_payloads = slack_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("slack", slack_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("slack", slack_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_slack_chunks(store, slack_payloads).await;
    }
    if send_message_targets_mattermost(payload) {
        let mattermost_payloads = mattermost_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("mattermost", mattermost_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("mattermost", mattermost_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_mattermost_chunks(store, mattermost_payloads).await;
    }
    if send_message_targets_matrix(payload) {
        let matrix_payloads = matrix_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("matrix", matrix_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("matrix", matrix_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_matrix_chunks(store, matrix_payloads).await;
    }
    if send_message_targets_signal(payload) {
        let signal_payloads = signal_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("signal", signal_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("signal", signal_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_signal_chunks(store, signal_payloads).await;
    }
    if send_message_targets_email(payload) {
        let email_payloads = email_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("email", email_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("email", email_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_email_chunks(store, email_payloads).await;
    }
    if send_message_targets_sms(payload) {
        let sms_payloads = sms_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("sms", sms_payloads.first()) {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("sms", sms_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_sms_chunks(store, sms_payloads).await;
    }
    if send_message_targets_dingtalk(payload) {
        let dingtalk_payloads = dingtalk_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("dingtalk", dingtalk_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("dingtalk", dingtalk_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_dingtalk_chunks(store, dingtalk_payloads).await;
    }
    if send_message_targets_teams(payload) {
        let teams_payloads = teams_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("teams", teams_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("teams", teams_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_teams_chunks(store, teams_payloads).await;
    }
    if send_message_targets_ntfy(payload) {
        let ntfy_payloads = ntfy_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("ntfy", ntfy_payloads.first()) {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("ntfy", ntfy_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_ntfy_chunks(store, ntfy_payloads).await;
    }
    if send_message_targets_simplex(payload) {
        let simplex_payloads = simplex_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("simplex", simplex_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("simplex", simplex_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_simplex_chunks(store, simplex_payloads).await;
    }
    if send_message_targets_irc(payload) {
        let irc_payloads = irc_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("irc", irc_payloads.first()) {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("irc", irc_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_irc_chunks(store, irc_payloads).await;
    }
    if send_message_targets_line(payload) {
        let line_payloads = line_send_message_payloads(store, current_conversation_id, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("line", line_payloads.first()) {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("line", line_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_line_chunks(store, line_payloads).await;
    }
    if send_message_targets_google_chat(payload) {
        let google_chat_payloads =
            google_chat_send_message_payloads(store, current_conversation_id, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("google_chat", google_chat_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("google_chat", google_chat_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_google_chat_chunks(store, google_chat_payloads).await;
    }
    if send_message_targets_whatsapp(payload) {
        let whatsapp_payloads = whatsapp_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("whatsapp", whatsapp_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("whatsapp", whatsapp_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_whatsapp_chunks(store, whatsapp_payloads).await;
    }
    if send_message_targets_qqbot(payload) {
        let qqbot_payloads = qqbot_send_message_payloads(store, payload)?;
        if let Some(skipped) = send_message_silence_narration_skip("qqbot", qqbot_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) = send_message_cron_duplicate_skip("qqbot", qqbot_payloads.first()) {
            return Ok(skipped);
        }
        return send_message_qqbot_chunks(store, qqbot_payloads).await;
    }
    if send_message_targets_homeassistant(payload) {
        let homeassistant_payloads = homeassistant_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("homeassistant", homeassistant_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("homeassistant", homeassistant_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_homeassistant_chunks(store, homeassistant_payloads).await;
    }
    if send_message_targets_bluebubbles(payload) {
        let bluebubbles_payloads = bluebubbles_send_message_payloads(store, payload)?;
        if let Some(skipped) =
            send_message_silence_narration_skip("bluebubbles", bluebubbles_payloads.first())
        {
            return Ok(skipped);
        }
        if let Some(skipped) =
            send_message_cron_duplicate_skip("bluebubbles", bluebubbles_payloads.first())
        {
            return Ok(skipped);
        }
        return send_message_bluebubbles_chunks(store, bluebubbles_payloads).await;
    }
    send_message_tool(store, current_conversation_id, payload)
}

fn send_message_action(payload: &Value) -> String {
    payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("send")
        .trim()
        .to_lowercase()
}

pub(super) fn send_message_silence_narration_skip(
    platform: &str,
    payload: Option<&Value>,
) -> Option<String> {
    let payload = payload?;
    if !send_message_silence_narration_filter_enabled() {
        return None;
    }
    if send_message_payload_has_media(payload) {
        return None;
    }
    let content = string_arg(
        payload,
        &["message", "text", "content", "body", "markdown", "title"],
    )?;
    if !is_silence_narration(&content) {
        return None;
    }
    Some(
        serde_json::to_string_pretty(&json!({
            "success": true,
            "platform": platform,
            "filtered": "silence_narration",
            "filteredReason": "hermes_delivery_silence_narration",
            "delivered": false,
            "gateway": true,
        }))
        .unwrap_or_else(|_| {
            "{\"success\":true,\"filtered\":\"silence_narration\",\"delivered\":false}".into()
        }),
    )
}

fn send_message_silence_narration_filter_enabled() -> bool {
    for key in [
        "HERMES_FILTER_SILENCE_NARRATION",
        "SYNTHCHAT_FILTER_SILENCE_NARRATION",
    ] {
        if let Ok(value) = env::var(key) {
            let normalized = value.trim().to_ascii_lowercase();
            return matches!(normalized.as_str(), "1" | "true" | "yes" | "on");
        }
    }
    true
}

fn send_message_payload_has_media(payload: &Value) -> bool {
    for key in ["media_files", "mediaFiles", "attachments", "files"] {
        if payload
            .get(key)
            .and_then(Value::as_array)
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn is_silence_narration(content: &str) -> bool {
    let stripped = content.trim();
    if stripped.is_empty() || stripped.chars().count() > 64 {
        return false;
    }
    let trimmed = stripped.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '*' | '_' | '~' | '`' | '(' | ')')
    });
    if trimmed.is_empty() {
        return false;
    }
    let lowered = trimmed.trim_end_matches('.').trim().to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "silent" | "silence" | "no response" | "no reply"
    ) {
        return true;
    }
    trimmed
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '.' | '\u{2026}' | '\u{1f507}'))
}

fn send_message_cron_auto_delivery_target() -> Option<(String, String, Option<String>)> {
    let platform = env::var("HERMES_CRON_AUTO_DELIVER_PLATFORM")
        .ok()
        .or_else(|| env::var("SYNTHCHAT_CRON_AUTO_DELIVER_PLATFORM").ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())?;
    let chat_id = env::var("HERMES_CRON_AUTO_DELIVER_CHAT_ID")
        .ok()
        .or_else(|| env::var("SYNTHCHAT_CRON_AUTO_DELIVER_CHAT_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let thread_id = env::var("HERMES_CRON_AUTO_DELIVER_THREAD_ID")
        .ok()
        .or_else(|| env::var("SYNTHCHAT_CRON_AUTO_DELIVER_THREAD_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Some((platform, chat_id, thread_id))
}

fn send_message_payload_target_id(platform: &str, payload: &Value) -> Option<String> {
    let keys: &[&str] = match platform {
        "discord" | "slack" | "mattermost" => &["channel_id", "channelId", "chat_id", "chatId"],
        "feishu" => &["receive_id", "receiveId", "chat_id", "chatId"],
        "telegram" | "whatsapp" | "qqbot" | "bluebubbles" | "wecom" | "weixin" => {
            &["chat_id", "chatId", "channel_id", "channelId"]
        }
        "matrix" => &["room_id", "roomId", "chat_id", "chatId"],
        "signal" => &["recipient", "chat_id", "chatId"],
        "email" | "sms" => &["to", "recipient", "chat_id", "chatId"],
        "dingtalk" => &["target", "chat_id", "chatId"],
        "teams" => &["chat_id", "chatId", "channel_id", "channelId", "to"],
        "ntfy" => &["topic", "chat_id", "chatId", "channel_id", "channelId"],
        "simplex" => &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "recipient",
            "to",
        ],
        "irc" => &[
            "chat_id",
            "chatId",
            "channel",
            "channel_id",
            "channelId",
            "to",
        ],
        "line" => &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "to",
            "recipient",
        ],
        "google_chat" => &[
            "chat_id",
            "chatId",
            "space",
            "spaceName",
            "user",
            "userName",
            "to",
        ],
        "homeassistant" => &[
            "notify_target",
            "notifyTarget",
            "target",
            "chat_id",
            "chatId",
        ],
        "yuanbao" => &["chat_id", "chatId", "user_id", "userId"],
        _ => &["chat_id", "chatId", "channel_id", "channelId", "target"],
    };
    string_arg(payload, keys)
}

fn send_message_payload_thread_id(platform: &str, payload: &Value) -> Option<String> {
    let keys: &[&str] = match platform {
        "telegram" => &[
            "thread_id",
            "threadId",
            "message_thread_id",
            "messageThreadId",
        ],
        "slack" => &["thread_ts", "threadTs", "thread_id", "threadId"],
        "mattermost" => &["root_id", "rootId", "thread_id", "threadId"],
        "discord" | "matrix" | "feishu" => &["thread_id", "threadId"],
        _ => &["thread_id", "threadId"],
    };
    string_arg(payload, keys)
}

fn send_message_cron_duplicate_skip(platform: &str, payload: Option<&Value>) -> Option<String> {
    let payload = payload?;
    let platform = platform.trim().to_ascii_lowercase();
    let (auto_platform, auto_chat_id, auto_thread_id) = send_message_cron_auto_delivery_target()?;
    if auto_platform != platform {
        return None;
    }
    let chat_id = send_message_payload_target_id(&platform, payload)?;
    if chat_id != auto_chat_id {
        return None;
    }
    let thread_id = send_message_payload_thread_id(&platform, payload);
    if thread_id != auto_thread_id {
        return None;
    }
    let mut target_label = format!("{platform}:{chat_id}");
    if let Some(thread_id) = thread_id {
        target_label = format!("{target_label}:{thread_id}");
    }
    Some(
        serde_json::to_string_pretty(&json!({
            "success": true,
            "skipped": true,
            "reason": "cron_auto_delivery_duplicate_target",
            "target": target_label,
            "note": "Skipped send_message because this cron job will already auto-deliver its final response to the same target."
        }))
        .ok()?,
    )
}

fn send_message_delivery_metadata(
    platform: &str,
    payloads: &[Value],
    results: &[Value],
    transport: &str,
) -> Value {
    let first_payload = payloads.first();
    let delivered = results.iter().all(|result| {
        result.get("error").is_none()
            && result.get("success").and_then(Value::as_bool) != Some(false)
    });
    let message_ids = results
        .iter()
        .filter_map(send_message_result_message_id)
        .collect::<Vec<_>>();
    let target_id =
        first_payload.and_then(|payload| send_message_payload_target_id(platform, payload));
    json!({
        "schema": "hermes_post_delivery_callback_desktop_v1",
        "delivered": delivered,
        "transport": transport,
        "platform": platform,
        "target": target_id,
        "targetId": target_id,
        "target_id": target_id,
        "threadId": first_payload.and_then(|payload| send_message_payload_thread_id(platform, payload)),
        "thread_id": first_payload.and_then(|payload| send_message_payload_thread_id(platform, payload)),
        "messageId": message_ids.first().cloned(),
        "message_id": message_ids.first().cloned(),
        "messageIds": message_ids,
        "message_ids": message_ids,
        "chunkCount": results.len(),
        "chunk_count": results.len(),
        "messageSource": "send_message",
        "message_source": "send_message",
        "desktopAdaptation": true,
    })
}

fn send_message_result_message_id(result: &Value) -> Option<String> {
    for key in [
        "messageId",
        "message_id",
        "id",
        "ts",
        "event_id",
        "eventId",
        "sid",
        "msg_id",
        "msgId",
    ] {
        if let Some(value) = result.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    for key in ["raw", "result", "message", "data"] {
        if let Some(found) = result.get(key).and_then(send_message_result_message_id) {
            return Some(found);
        }
    }
    result
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| results.iter().find_map(send_message_result_message_id))
}

pub(super) fn format_send_message_delivery_result(
    platform: &str,
    payloads: &[Value],
    results: Vec<Value>,
    transport: &str,
    mut extra: Value,
) -> AppResult<String> {
    let multi = results.len() > 1;
    let post_delivery = send_message_delivery_metadata(platform, payloads, &results, transport);
    if !multi {
        let mut result = results.first().cloned().unwrap_or_else(|| json!({}));
        if !result.is_object() {
            result = json!({"raw": result});
        }
        result["postDelivery"] = post_delivery.clone();
        result["post_delivery"] = post_delivery;
        return Ok(serde_json::to_string_pretty(&result)?);
    }
    if !extra.is_object() {
        extra = json!({});
    }
    extra["success"] = json!(true);
    extra["platform"] = json!(platform);
    extra["chunks"] = json!(results.len());
    extra["results"] = json!(results);
    extra["postDelivery"] = post_delivery.clone();
    extra["post_delivery"] = post_delivery;
    Ok(serde_json::to_string_pretty(&extra)?)
}

async fn send_message_discord_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = discord_tool(store, "discord", &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("discord", &payloads, results, "discord", json!({}))
}

async fn send_message_feishu_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = feishu_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("feishu", &payloads, results, "feishu", json!({}))
}

async fn send_message_yuanbao_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = yuanbao_tool(store, "yb_send_dm", &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("yuanbao", &payloads, results, "yuanbao", json!({}))
}

async fn send_message_telegram_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    let mut fallback_without_thread = false;
    let mut effective_payloads = Vec::new();
    for mut payload in payloads.clone() {
        if fallback_without_thread {
            telegram_clear_thread_fields(&mut payload);
        }
        let text = telegram_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if telegram_result_used_thread_fallback(&value) {
                fallback_without_thread = true;
            }
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
        effective_payloads.push(payload);
    }
    format_send_message_delivery_result(
        "telegram",
        &effective_payloads,
        results,
        "telegram",
        json!({"threadFallbackWithoutThread": fallback_without_thread}),
    )
}

pub(super) fn telegram_clear_thread_fields(payload: &mut Value) {
    if let Some(object) = payload.as_object_mut() {
        for key in [
            "thread_id",
            "threadId",
            "message_thread_id",
            "messageThreadId",
        ] {
            object.remove(key);
        }
    }
}

pub(super) fn telegram_result_used_thread_fallback(value: &Value) -> bool {
    value
        .get("telegram_thread_fallback_without_thread")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("results")
            .and_then(Value::as_array)
            .map(|results| results.iter().any(telegram_result_used_thread_fallback))
            .unwrap_or(false)
}

async fn send_message_slack_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = slack_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("slack", &payloads, results, "slack", json!({}))
}

async fn send_message_mattermost_chunks(
    store: &AppStore,
    payloads: Vec<Value>,
) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = mattermost_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("mattermost", &payloads, results, "mattermost", json!({}))
}

async fn send_message_matrix_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = matrix_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("matrix", &payloads, results, "matrix", json!({}))
}

async fn send_message_signal_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = signal_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("signal", &payloads, results, "signal", json!({}))
}

async fn send_message_email_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = email_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("email", &payloads, results, "email", json!({}))
}

async fn send_message_sms_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = sms_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("sms", &payloads, results, "sms", json!({}))
}

async fn send_message_dingtalk_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = dingtalk_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("dingtalk", &payloads, results, "dingtalk", json!({}))
}

async fn send_message_teams_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = teams_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("teams", &payloads, results, "teams", json!({}))
}

async fn send_message_ntfy_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = ntfy_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("ntfy", &payloads, results, "ntfy", json!({}))
}

async fn send_message_simplex_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = simplex_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("simplex", &payloads, results, "simplex", json!({}))
}

async fn send_message_irc_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = irc_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("irc", &payloads, results, "irc", json!({}))
}

async fn send_message_line_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        if let Some(request_id) = string_arg(
            payload,
            &[
                "linePostbackRequestId",
                "line_postback_request_id",
                "postbackRequestId",
                "postback_request_id",
            ],
        ) {
            let message =
                string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
            line_postback_cache_set_ready(store, &request_id, &message)?;
            results.push(json!({
                "success": true,
                "platform": "line",
                "delivery_mode": "postback_cache",
                "deliveryMode": "postback_cache",
                "message_id": request_id,
                "messageId": request_id,
                "cached": true,
            }));
            continue;
        }
        let text = line_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("line", &payloads, results, "line", json!({}))
}

async fn send_message_google_chat_chunks(
    store: &AppStore,
    payloads: Vec<Value>,
) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = google_chat_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("google_chat", &payloads, results, "google_chat", json!({}))
}

async fn send_message_whatsapp_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = whatsapp_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("whatsapp", &payloads, results, "whatsapp", json!({}))
}

async fn send_message_qqbot_chunks(store: &AppStore, payloads: Vec<Value>) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = qqbot_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("qqbot", &payloads, results, "qqbot", json!({}))
}

async fn send_message_homeassistant_chunks(
    store: &AppStore,
    payloads: Vec<Value>,
) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = homeassistant_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result(
        "homeassistant",
        &payloads,
        results,
        "homeassistant",
        json!({}),
    )
}

async fn send_message_bluebubbles_chunks(
    store: &AppStore,
    payloads: Vec<Value>,
) -> AppResult<String> {
    let mut results = Vec::new();
    for payload in &payloads {
        let text = bluebubbles_send_message_tool(store, &payload).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result("bluebubbles", &payloads, results, "bluebubbles", json!({}))
}

async fn send_message_messaging_gateway_chunks(
    store: &AppStore,
    payloads: Vec<Value>,
) -> AppResult<String> {
    let platform = payloads
        .first()
        .and_then(|payload| payload.get("platform"))
        .and_then(Value::as_str)
        .unwrap_or("gateway")
        .to_string();
    let mut results = Vec::new();
    for payload in &payloads {
        let text = messaging_gateway_send_message_tool(store, &payload).await?;
        let _ = delivery_mirror_to_session(store, &platform, &payload, "messaging_gateway");
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            results.push(value);
        } else {
            results.push(json!({ "raw": text }));
        }
    }
    format_send_message_delivery_result(
        &platform,
        &payloads,
        results,
        "messaging_gateway",
        json!({"gateway": true}),
    )
}

pub(super) fn delivery_mirror_to_session(
    store: &AppStore,
    platform: &str,
    payload: &Value,
    source_label: &str,
) -> bool {
    let Some(chat_id) = string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "room_id",
            "roomId",
            "target",
            "to",
            "recipient",
        ],
    ) else {
        return false;
    };
    let message_text = string_arg(
        payload,
        &["message", "text", "content", "body", "markdown", "title"],
    )
    .unwrap_or_else(|| {
        payload
            .get("media_files")
            .or_else(|| payload.get("mediaFiles"))
            .and_then(Value::as_array)
            .map(|items| format!("[media attachments: {}]", items.len()))
            .unwrap_or_default()
    });
    if message_text.trim().is_empty() {
        return false;
    }
    let thread_id = string_arg(payload, &["thread_id", "threadId"]);
    let user_id = string_arg(payload, &["user_id", "userId"]);
    let Some(conversation_id) = delivery_mirror_find_conversation(
        store,
        platform,
        &chat_id,
        thread_id.as_deref(),
        user_id.as_deref(),
    ) else {
        return false;
    };
    let mut message = ChatMessage::new(
        conversation_id,
        "assistant",
        message_text,
        "delivery-mirror",
    );
    message.provider_data = Some(json!({
        "mirror": true,
        "mirrorSource": source_label,
        "mirror_source": source_label,
        "platform": platform,
        "chatId": chat_id,
        "chat_id": chat_id,
        "threadId": thread_id,
        "thread_id": thread_id,
        "userId": user_id,
        "user_id": user_id,
    }));
    store.append_message(message).is_ok()
}

fn delivery_mirror_find_conversation(
    store: &AppStore,
    platform: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    user_id: Option<&str>,
) -> Option<String> {
    let conversations = store.conversations().ok()?;
    let mut candidates = conversations
        .into_iter()
        .filter(|conversation| {
            delivery_mirror_metadata_matches(
                &conversation.metadata,
                platform,
                chat_id,
                thread_id,
                None,
            )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    if let Some(user_id) = user_id {
        let exact_user_matches = candidates
            .iter()
            .filter(|conversation| {
                delivery_mirror_metadata_matches(
                    &conversation.metadata,
                    platform,
                    chat_id,
                    thread_id,
                    Some(user_id),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if !exact_user_matches.is_empty() {
            candidates = exact_user_matches;
        } else if candidates.len() > 1 {
            return None;
        }
    } else {
        let distinct_users = candidates
            .iter()
            .filter_map(|conversation| {
                delivery_mirror_origin_string(&conversation.metadata, &["userId", "user_id"])
            })
            .filter(|value| !value.trim().is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        if distinct_users.len() > 1 {
            return None;
        }
    }
    candidates.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    candidates
        .first()
        .map(|conversation| conversation.id.clone())
}

fn delivery_mirror_metadata_matches(
    metadata: &Value,
    platform: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    user_id: Option<&str>,
) -> bool {
    let origin = metadata
        .get("origin")
        .or_else(|| metadata.get("source"))
        .unwrap_or(metadata);
    let origin_platform = delivery_mirror_metadata_string(origin, &["platform"])
        .or_else(|| delivery_mirror_metadata_string(metadata, &["platform"]))
        .unwrap_or_default();
    if !origin_platform.eq_ignore_ascii_case(platform) {
        return false;
    }
    let Some(origin_chat_id) = delivery_mirror_metadata_string(
        origin,
        &[
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
            "roomId",
            "room_id",
        ],
    )
    .or_else(|| {
        delivery_mirror_metadata_string(
            metadata,
            &[
                "chatId",
                "chat_id",
                "channelId",
                "channel_id",
                "roomId",
                "room_id",
            ],
        )
    }) else {
        return false;
    };
    if origin_chat_id != chat_id {
        return false;
    }
    if let Some(thread_id) = thread_id {
        let Some(origin_thread_id) =
            delivery_mirror_metadata_string(origin, &["threadId", "thread_id"])
                .or_else(|| delivery_mirror_metadata_string(metadata, &["threadId", "thread_id"]))
        else {
            return false;
        };
        if origin_thread_id != thread_id {
            return false;
        }
    }
    if let Some(user_id) = user_id {
        let Some(origin_user_id) = delivery_mirror_metadata_string(origin, &["userId", "user_id"])
            .or_else(|| delivery_mirror_metadata_string(metadata, &["userId", "user_id"]))
        else {
            return false;
        };
        if origin_user_id != user_id {
            return false;
        }
    }
    true
}

fn delivery_mirror_metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

fn delivery_mirror_origin_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    let origin = metadata
        .get("origin")
        .or_else(|| metadata.get("source"))
        .unwrap_or(metadata);
    delivery_mirror_metadata_string(origin, keys)
        .or_else(|| delivery_mirror_metadata_string(metadata, keys))
}

fn send_message_targets_discord(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("discord"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "discord" || target.starts_with("discord:")
            })
            .unwrap_or(false)
}

fn send_message_targets_email(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("email"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "email" || target.starts_with("email:")
            })
            .unwrap_or(false)
}

fn send_message_targets_sms(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("sms"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "sms" || target.starts_with("sms:")
            })
            .unwrap_or(false)
}

fn send_message_targets_dingtalk(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("dingtalk"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "dingtalk" || target.starts_with("dingtalk:")
            })
            .unwrap_or(false)
}

fn send_message_targets_teams(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "teams" || value == "microsoft_teams" || value == "microsoft-teams"
        })
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "teams"
                    || target == "microsoft_teams"
                    || target == "microsoft-teams"
                    || target.starts_with("teams:")
                    || target.starts_with("microsoft_teams:")
                    || target.starts_with("microsoft-teams:")
            })
            .unwrap_or(false)
}

fn send_message_targets_ntfy(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("ntfy"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "ntfy" || target.starts_with("ntfy:")
            })
            .unwrap_or(false)
}

fn send_message_targets_simplex(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("simplex"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "simplex" || target.starts_with("simplex:")
            })
            .unwrap_or(false)
}

fn send_message_targets_irc(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("irc"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "irc" || target.starts_with("irc:")
            })
            .unwrap_or(false)
}

fn send_message_targets_line(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("line"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "line" || target.starts_with("line:")
            })
            .unwrap_or(false)
}

fn send_message_targets_google_chat(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("google_chat"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "google_chat"
                    || target.starts_with("google_chat:")
                    || target == "gchat"
                    || target.starts_with("gchat:")
            })
            .unwrap_or(false)
}

fn send_message_targets_whatsapp(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("whatsapp"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "whatsapp" || target.starts_with("whatsapp:")
            })
            .unwrap_or(false)
}

fn send_message_targets_qqbot(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("qqbot"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "qqbot" || target.starts_with("qqbot:")
            })
            .unwrap_or(false)
}

fn send_message_targets_homeassistant(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "homeassistant" || value == "home_assistant"
        })
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "homeassistant"
                    || target == "home_assistant"
                    || target.starts_with("homeassistant:")
                    || target.starts_with("home_assistant:")
            })
            .unwrap_or(false)
}

fn send_message_targets_bluebubbles(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("bluebubbles"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "bluebubbles" || target.starts_with("bluebubbles:")
            })
            .unwrap_or(false)
}

pub(super) fn send_message_targets_messaging_gateway(
    store: &AppStore,
    payload: &Value,
) -> AppResult<bool> {
    let config = store.config()?;
    if !messaging_gateway_configured(&config.messaging_gateway) {
        return Ok(false);
    }
    let Some(platform) = messaging_gateway_payload_platform(payload) else {
        return Ok(false);
    };
    if platform == "yuanbao" {
        return Ok(messaging_gateway_payload_is_yuanbao_group(payload)
            && messaging_gateway_platform_enabled(&config.messaging_gateway, "yuanbao"));
    }
    Ok(matches!(platform.as_str(), "wecom" | "weixin")
        && messaging_gateway_platform_enabled(&config.messaging_gateway, &platform))
}

fn messaging_gateway_payload_platform(payload: &Value) -> Option<String> {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            string_arg(payload, &["target", "targetId", "target_id"]).and_then(|target| {
                let trimmed = target.trim();
                if trimmed.to_ascii_lowercase().starts_with("group:") {
                    return Some("yuanbao".into());
                }
                target
                    .split_once(':')
                    .map(|(platform, _)| platform.trim().to_ascii_lowercase())
                    .filter(|platform| !platform.is_empty())
            })
        })
}

fn messaging_gateway_payload_is_yuanbao_group(payload: &Value) -> bool {
    string_arg(payload, &["target", "targetId", "target_id"])
        .map(|target| {
            let target = target.trim().to_ascii_lowercase();
            target.starts_with("yuanbao:group:") || target.starts_with("group:")
        })
        .unwrap_or(false)
        || string_arg(payload, &["chat_id", "chatId"])
            .map(|chat_id| chat_id.trim().to_ascii_lowercase().starts_with("group:"))
            .unwrap_or(false)
}

pub(super) fn messaging_gateway_send_message_payloads(payload: &Value) -> AppResult<Vec<Value>> {
    let platform = messaging_gateway_payload_platform(payload).ok_or_else(|| {
        AppError::BadRequest(
            "send_message messaging gateway requires target \"wecom:<id>\", \"weixin:<id>\", \"yuanbao:group:<id>\", or payload.platform"
                .into(),
        )
    })?;
    let raw_target = string_arg(payload, &["target", "targetId", "target_id"])
        .or_else(|| {
            string_arg(payload, &["chat_id", "chatId"])
                .map(|chat_id| format!("{platform}:{chat_id}"))
        })
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message messaging gateway requires payload.target or payload.chat_id".into(),
            )
        })?;
    let chat_id = messaging_gateway_chat_id(&platform, &raw_target)
        .or_else(|| string_arg(payload, &["chat_id", "chatId", "to", "recipient"]))
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message messaging gateway requires a non-empty platform target".into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let limit = match platform.as_str() {
        "weixin" | "yuanbao" => 2_000,
        "wecom" => 4_000,
        _ => 4_000,
    };
    let chunks = chunk_message_text(&message, limit);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "platform": platform.clone(),
                "target": raw_target.clone(),
                "chat_id": chat_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn messaging_gateway_chat_id(platform: &str, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let lower_target = target.to_ascii_lowercase();
    let lower_prefix = format!("{platform}:");
    let rest = if lower_target.starts_with(&lower_prefix) {
        &target[lower_prefix.len()..]
    } else {
        target
    }
    .trim();
    if rest.is_empty() {
        None
    } else {
        Some(resolve_channel_directory_target(platform, rest).unwrap_or_else(|| rest.to_string()))
    }
}

fn send_message_targets_telegram(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("telegram"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "telegram" || target.starts_with("telegram:")
            })
            .unwrap_or(false)
}

fn send_message_targets_slack(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("slack"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "slack" || target.starts_with("slack:")
            })
            .unwrap_or(false)
}

fn send_message_targets_mattermost(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("mattermost"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "mattermost" || target.starts_with("mattermost:")
            })
            .unwrap_or(false)
}

fn send_message_targets_matrix(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("matrix"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "matrix" || target.starts_with("matrix:")
            })
            .unwrap_or(false)
}

fn send_message_targets_signal(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("signal"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "signal" || target.starts_with("signal:")
            })
            .unwrap_or(false)
}

pub(super) fn signal_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_recipient = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_signal_send_message_target(&target));
    let recipient = string_arg(
        payload,
        &["recipient", "recipient_id", "recipientId", "chat_id", "chatId"],
    )
    .or(target_recipient)
    .or_else(|| signal_home_recipient_id(store).ok().flatten())
    .ok_or_else(|| {
        AppError::BadRequest(
            "send_message to Signal requires payload.recipient, target \"signal:<recipient>\", or settings.signal.homeRecipient"
                .into(),
        )
    })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 8_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "recipient": recipient.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

pub(super) fn email_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_address = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_email_send_message_target(&target));
    let to = string_arg(payload, &["to", "email", "address", "recipient"])
        .or(target_address)
        .or_else(|| email_home_address(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Email requires payload.to, target \"email:<address>\", or settings.email.homeAddress"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let subject = string_arg(payload, &["subject", "title"]);
    let chunks = chunk_message_text(&message, 20_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "to": to.clone(),
                "message": chunk,
                "subject": subject.clone(),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

pub(super) fn sms_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_number = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_sms_send_message_target(&target));
    let to = string_arg(payload, &["to", "phone", "number", "recipient", "chat_id", "chatId"])
        .or(target_number)
        .or_else(|| sms_home_number(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to SMS requires payload.to, target \"sms:<phone>\", or settings.sms.homeNumber"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_urls_value = payload
        .get("media_urls")
        .or_else(|| payload.get("mediaUrls"))
        .or_else(|| payload.get("media_url"))
        .or_else(|| payload.get("mediaUrl"))
        .or_else(|| payload.get("media_files"))
        .or_else(|| payload.get("mediaFiles"))
        .cloned()
        .unwrap_or_else(|| Value::Array(extracted_media_files));
    let media_urls = sms_media_urls(media_urls_value)?;
    let chunks = chunk_message_text(&message, 1_600);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "to": to.clone(),
                "message": chunk,
                "media_urls": if index == last_index { Value::Array(media_urls.clone()) } else { json!([]) },
            })
        })
        .collect())
}

fn sms_media_urls(files: Value) -> AppResult<Vec<Value>> {
    let files = if let Some(files) = files.as_array() {
        files.clone()
    } else if files.is_null() {
        Vec::new()
    } else {
        vec![files]
    };
    files
        .iter()
        .filter_map(|file| {
            let raw = file.as_str().map(str::to_string).or_else(|| {
                string_arg(file, &["url", "media_url", "mediaUrl", "path", "file", "file_path"])
            })?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .map(|url| {
            let parsed = reqwest::Url::parse(&url).map_err(|_| {
                AppError::BadRequest(
                    "send_message SMS MEDIA attachments must be public http(s) URLs for Twilio MediaUrl"
                        .into(),
                )
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(AppError::BadRequest(
                    "send_message SMS MEDIA attachments must be public http(s) URLs for Twilio MediaUrl"
                        .into(),
                ));
            }
            Ok(json!(url))
        })
        .collect()
}

pub(super) fn dingtalk_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_ref = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_dingtalk_send_message_target(&target))
        .or_else(|| dingtalk_home_target(store).ok().flatten())
        .unwrap_or_else(|| "webhook".into());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_urls_value = payload
        .get("media_urls")
        .or_else(|| payload.get("mediaUrls"))
        .or_else(|| payload.get("media_url"))
        .or_else(|| payload.get("mediaUrl"))
        .or_else(|| payload.get("media_files"))
        .or_else(|| payload.get("mediaFiles"))
        .cloned()
        .unwrap_or_else(|| Value::Array(extracted_media_files));
    let media_urls = dingtalk_media_urls(media_urls_value)?;
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "target": target_ref.clone(),
                "message": chunk,
                "media_urls": if index == last_index { Value::Array(media_urls.clone()) } else { json!([]) },
            })
        })
        .collect())
}

pub(super) fn teams_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_teams_send_message_target(&target));
    let chat_id = string_arg(
        payload,
        &["chat_id", "chatId", "channel_id", "channelId", "to"],
    )
    .or(target_chat_id)
    .or_else(|| teams_home_channel(store).ok().flatten());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    if payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .is_some()
        || payload
            .get("media_urls")
            .or_else(|| payload.get("mediaUrls"))
            .is_some()
        || !extracted_media_files.is_empty()
    {
        return Err(AppError::BadRequest(
            "send_message Teams desktop route does not support MEDIA attachments; use the live Hermes Teams SDK adapter for Bot Framework media delivery".into(),
        ));
    }
    let thread_id = string_arg(payload, &["thread_id", "threadId", "reply_to", "replyTo"]);
    let chunks = chunk_message_text(&message, 28_000);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "chat_id": chat_id.clone(),
                "thread_id": thread_id.clone(),
                "message": chunk,
            })
        })
        .collect())
}

pub(super) fn ntfy_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_topic = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_ntfy_send_message_target(&target));
    let topic = string_arg(
        payload,
        &["topic", "chat_id", "chatId", "channel_id", "channelId"],
    )
    .or(target_topic)
    .or_else(|| ntfy_home_channel(store).ok().flatten());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    if payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .is_some()
        || payload
            .get("media_urls")
            .or_else(|| payload.get("mediaUrls"))
            .is_some()
        || !extracted_media_files.is_empty()
    {
        return Err(AppError::BadRequest(
            "send_message ntfy does not support MEDIA attachments".into(),
        ));
    }
    let chunks = chunk_message_text(&message, 4096);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "topic": topic.clone(),
                "message": chunk,
            })
        })
        .collect())
}

pub(super) fn simplex_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_simplex_send_message_target(&target));
    let chat_id = string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "recipient",
            "to",
        ],
    )
    .or(target_chat_id)
    .or_else(|| simplex_home_channel(store).ok().flatten());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    if payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .is_some()
        || payload
            .get("media_urls")
            .or_else(|| payload.get("mediaUrls"))
            .is_some()
        || !extracted_media_files.is_empty()
    {
        return Err(AppError::BadRequest(
            "send_message SimpleX standalone route does not support MEDIA attachments".into(),
        ));
    }
    let chunks = chunk_message_text(&message, 16_000);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "chat_id": chat_id.clone(),
                "message": chunk,
            })
        })
        .collect())
}

pub(super) fn irc_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_irc_send_message_target(&target));
    let channel = string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "channel",
            "channel_id",
            "channelId",
            "to",
        ],
    )
    .or(target_chat_id)
    .or_else(|| irc_home_channel(store).ok().flatten());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    if payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .is_some()
        || payload
            .get("media_urls")
            .or_else(|| payload.get("mediaUrls"))
            .is_some()
        || !extracted_media_files.is_empty()
    {
        return Err(AppError::BadRequest(
            "send_message IRC does not support MEDIA attachments".into(),
        ));
    }
    let chunks = chunk_message_text(&message, 450);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "channel": channel.clone(),
                "message": chunk,
            })
        })
        .collect())
}

pub(super) fn line_send_message_payloads(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let origin = line_current_conversation_origin(store, current_conversation_id);
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_line_send_message_target(&target));
    let chat_id = string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "to",
            "recipient",
        ],
    )
    .or(target_chat_id)
    .or_else(|| {
        origin
            .as_ref()
            .and_then(|origin| string_arg(origin, &["chatId", "chat_id"]))
    })
    .or_else(|| line_home_channel(store).ok().flatten());
    let reply_token = string_arg(payload, &["replyToken", "reply_token"]).or_else(|| {
        origin
            .as_ref()
            .and_then(|origin| string_arg(origin, &["replyToken", "reply_token"]))
    });
    let postback_request_id =
        line_postback_pending_request_for_conversation(store, current_conversation_id)?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_500);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "chat_id": chat_id.clone(),
                "replyToken": reply_token.clone().map(Value::String).unwrap_or(Value::Null),
                "reply_token": reply_token.clone().map(Value::String).unwrap_or(Value::Null),
                "linePostbackRequestId": postback_request_id.clone().map(Value::String).unwrap_or(Value::Null),
                "line_postback_request_id": postback_request_id.clone().map(Value::String).unwrap_or(Value::Null),
                "message": chunk,
                "media_files": media_files.clone(),
            })
        })
        .collect())
}

pub(super) fn google_chat_send_message_payloads(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let origin = google_chat_current_conversation_origin(store, current_conversation_id);
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_google_chat_send_message_target(&target));
    let chat_id = string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "space",
            "spaceName",
            "user",
            "userName",
            "to",
        ],
    )
    .or(target_chat_id)
    .or_else(|| {
        origin
            .as_ref()
            .and_then(|origin| delivery_mirror_metadata_string(origin, &["chatId", "chat_id"]))
    })
    .or_else(|| google_chat_home_channel(store).ok().flatten());
    let thread_id = string_arg(payload, &["thread_id", "threadId", "thread"]).or_else(|| {
        origin
            .as_ref()
            .and_then(|origin| delivery_mirror_metadata_string(origin, &["threadId", "thread_id"]))
    });
    let sender_email = string_arg(
        payload,
        &["sender_email", "senderEmail", "user_email", "userEmail"],
    )
    .or_else(|| {
        origin
            .as_ref()
            .and_then(|origin| delivery_mirror_metadata_string(origin, &["userId", "user_id"]))
            .filter(|value| value.contains('@'))
    });
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "chat_id": chat_id.clone(),
                "thread_id": thread_id.clone(),
                "sender_email": sender_email.clone(),
                "message": chunk,
                "media_files": media_files.clone(),
            })
        })
        .collect())
}

fn google_chat_current_conversation_origin(
    store: &AppStore,
    current_conversation_id: &str,
) -> Option<Value> {
    let conversation = store.conversation(current_conversation_id).ok()?;
    let origin = conversation
        .metadata
        .get("origin")
        .or_else(|| conversation.metadata.get("source"))
        .unwrap_or(&conversation.metadata);
    let platform = delivery_mirror_metadata_string(origin, &["platform"])
        .or_else(|| delivery_mirror_metadata_string(&conversation.metadata, &["platform"]))?;
    platform
        .eq_ignore_ascii_case("google_chat")
        .then(|| origin.clone())
}

fn line_current_conversation_origin(
    store: &AppStore,
    current_conversation_id: &str,
) -> Option<Value> {
    let conversation = store.conversation(current_conversation_id).ok()?;
    let origin = conversation
        .metadata
        .get("origin")
        .or_else(|| conversation.metadata.get("source"))
        .unwrap_or(&conversation.metadata);
    let platform = delivery_mirror_metadata_string(origin, &["platform"])
        .or_else(|| delivery_mirror_metadata_string(&conversation.metadata, &["platform"]))?;
    platform
        .eq_ignore_ascii_case("line")
        .then(|| origin.clone())
}

fn dingtalk_media_urls(files: Value) -> AppResult<Vec<Value>> {
    let files = if let Some(files) = files.as_array() {
        files.clone()
    } else if files.is_null() {
        Vec::new()
    } else {
        vec![files]
    };
    files
        .iter()
        .filter_map(|file| {
            let raw = file.as_str().map(str::to_string).or_else(|| {
                string_arg(file, &["url", "media_url", "mediaUrl", "path", "file", "file_path"])
            })?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .map(|url| {
            let parsed = reqwest::Url::parse(&url).map_err(|_| {
                AppError::BadRequest(
                    "send_message DingTalk MEDIA attachments must be public http(s) image URLs for markdown rendering"
                        .into(),
                )
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(AppError::BadRequest(
                    "send_message DingTalk MEDIA attachments must be public http(s) image URLs for markdown rendering"
                        .into(),
                ));
            }
            Ok(json!(url))
        })
        .collect()
}

pub(super) fn whatsapp_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_whatsapp_send_message_target(&target));
    let chat_id = string_arg(payload, &["chat_id", "chatId", "to", "recipient"])
        .or(target_chat_id)
        .or_else(|| whatsapp_home_chat_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to WhatsApp requires payload.chat_id, target \"whatsapp:<chat_id>\", or settings.whatsapp.homeChatId"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "chat_id": chat_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

pub(super) fn qqbot_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_qqbot_send_message_target(&target));
    let chat_id = string_arg(
        payload,
        &["chat_id", "chatId", "channel_id", "channelId", "to", "recipient"],
    )
    .or(target_chat_id)
    .or_else(|| qqbot_home_target(store).ok().flatten())
    .ok_or_else(|| {
        AppError::BadRequest(
            "send_message to QQBot requires payload.chat_id, target \"qqbot:<id>\", or settings.qqbot.homeTarget"
                .into(),
        )
    })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "chat_id": chat_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

pub(super) fn homeassistant_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_homeassistant_send_message_target(&target))
        .or_else(|| {
            string_arg(
                payload,
                &["notifyTarget", "notify_target", "chat_id", "chatId"],
            )
        })
        .or_else(|| homeassistant_home_notify_target(store).ok().flatten());
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    if payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .is_some()
        || !extracted_media_files.is_empty()
    {
        return Err(AppError::BadRequest(
            "send_message Home Assistant notify routing does not support MEDIA attachments".into(),
        ));
    }
    let chunks = chunk_message_text(&message, 4_000);
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            json!({
                "notify_target": target.clone(),
                "message": chunk,
            })
        })
        .collect())
}

pub(super) fn bluebubbles_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_chat_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_bluebubbles_send_message_target(&target));
    let chat_id = string_arg(payload, &["chat_id", "chatId", "to", "recipient", "address"])
        .or(target_chat_id)
        .or_else(|| bluebubbles_home_chat_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to BlueBubbles requires payload.chat_id, target \"bluebubbles:<chat_id>\", or settings.bluebubbles.homeChatId"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "chat_id": chat_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_email_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("email:")
        .or_else(|| target.strip_prefix("Email:"))?;
    Some(resolve_channel_directory_target("email", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_sms_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("sms:")
        .or_else(|| target.strip_prefix("SMS:"))?;
    Some(resolve_channel_directory_target("sms", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_dingtalk_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("dingtalk:")
        .or_else(|| target.strip_prefix("DingTalk:"))?;
    Some(resolve_channel_directory_target("dingtalk", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_teams_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("teams:")
        .or_else(|| target.strip_prefix("Teams:"))
        .or_else(|| target.strip_prefix("microsoft_teams:"))
        .or_else(|| target.strip_prefix("Microsoft_Teams:"))
        .or_else(|| target.strip_prefix("microsoft-teams:"))
        .or_else(|| target.strip_prefix("Microsoft-Teams:"))?;
    Some(resolve_channel_directory_target("teams", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_ntfy_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("ntfy:")
        .or_else(|| target.strip_prefix("Ntfy:"))
        .or_else(|| target.strip_prefix("NTFY:"))?;
    Some(resolve_channel_directory_target("ntfy", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_simplex_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("simplex:")
        .or_else(|| target.strip_prefix("SimpleX:"))
        .or_else(|| target.strip_prefix("SIMPLEX:"))?;
    Some(resolve_channel_directory_target("simplex", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_irc_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("irc:")
        .or_else(|| target.strip_prefix("IRC:"))?;
    Some(resolve_channel_directory_target("irc", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_line_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("line:")
        .or_else(|| target.strip_prefix("LINE:"))?;
    Some(resolve_channel_directory_target("line", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_google_chat_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("google_chat:")
        .or_else(|| target.strip_prefix("GOOGLE_CHAT:"))
        .or_else(|| target.strip_prefix("gchat:"))
        .or_else(|| target.strip_prefix("GCHAT:"))?;
    Some(
        resolve_channel_directory_target("google_chat", rest).unwrap_or_else(|| rest.trim().into()),
    )
    .filter(|value| !value.trim().is_empty())
}

fn parse_whatsapp_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("whatsapp:")
        .or_else(|| target.strip_prefix("WhatsApp:"))?;
    Some(resolve_channel_directory_target("whatsapp", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_qqbot_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("qqbot:")
        .or_else(|| target.strip_prefix("QQBot:"))?;
    Some(resolve_channel_directory_target("qqbot", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

fn parse_homeassistant_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("homeassistant:")
        .or_else(|| target.strip_prefix("HomeAssistant:"))
        .or_else(|| target.strip_prefix("home_assistant:"))
        .or_else(|| target.strip_prefix("Home_Assistant:"))?;
    Some(
        resolve_channel_directory_target("homeassistant", rest)
            .unwrap_or_else(|| rest.trim().into()),
    )
    .filter(|value| !value.trim().is_empty())
}

fn parse_bluebubbles_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("bluebubbles:")
        .or_else(|| target.strip_prefix("BlueBubbles:"))?;
    Some(
        resolve_channel_directory_target("bluebubbles", rest).unwrap_or_else(|| rest.trim().into()),
    )
    .filter(|value| !value.trim().is_empty())
}

fn parse_signal_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("signal:")
        .or_else(|| target.strip_prefix("Signal:"))?;
    Some(resolve_channel_directory_target("signal", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn matrix_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let target_room_id = string_arg(payload, &["target", "targetId", "target_id"])
        .and_then(|target| parse_matrix_send_message_target(&target));
    let room_id = string_arg(payload, &["room_id", "roomId", "chat_id", "chatId"])
        .or(target_room_id)
        .or_else(|| matrix_home_room_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Matrix requires payload.room_id, target \"matrix:<room_id>\", or settings.matrix.homeRoom"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "room_id": room_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_matrix_send_message_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("matrix:")
        .or_else(|| target.strip_prefix("Matrix:"))?;
    Some(resolve_channel_directory_target("matrix", rest).unwrap_or_else(|| rest.trim().into()))
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn slack_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let (target_channel_id, target_thread_ts, target_user_id) =
        string_arg(payload, &["target", "targetId", "target_id"])
            .and_then(|target| parse_slack_send_message_target(&target))
            .unwrap_or((None, None, None));
    let channel_id = string_arg(payload, &["channel_id", "channelId", "chat_id", "chatId"])
        .or(target_channel_id)
        .or_else(|| slack_home_channel_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Slack requires payload.channel_id, target \"slack:<channel_id>\", or settings.slack.homeChannel"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let thread_ts = string_arg(
        payload,
        &[
            "thread_ts",
            "threadTs",
            "thread_id",
            "threadId",
            "message_id",
            "messageId",
        ],
    )
    .or(target_thread_ts)
    .or_else(|| slack_home_thread_ts(store).ok().flatten());
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "channel_id": channel_id.clone(),
                "message": chunk,
                "thread_ts": thread_ts.clone(),
                "slack_user_id": target_user_id.clone(),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_slack_send_message_target(
    target: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let target = target.trim();
    let rest = target
        .strip_prefix("slack:")
        .or_else(|| target.strip_prefix("Slack:"))?;
    let resolved = resolve_channel_directory_target("slack", rest).unwrap_or_else(|| rest.into());
    let rest = resolved.as_str();
    let mut parts = rest.splitn(2, ':');
    let channel_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let thread_ts = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let user_id = channel_id
        .as_deref()
        .filter(|value| slack_target_is_user_id(value))
        .map(str::to_string);
    Some((channel_id, thread_ts, user_id))
}

fn slack_target_is_user_id(value: &str) -> bool {
    let value = value.trim();
    value.len() >= 9
        && value.starts_with('U')
        && value
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

pub(super) fn mattermost_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let (target_channel_id, target_root_id) =
        string_arg(payload, &["target", "targetId", "target_id"])
            .and_then(|target| parse_mattermost_send_message_target(&target))
            .unwrap_or((None, None));
    let channel_id = string_arg(payload, &["channel_id", "channelId", "chat_id", "chatId"])
        .or(target_channel_id)
        .or_else(|| mattermost_home_channel_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Mattermost requires payload.channel_id, target \"mattermost:<channel_id>\", or settings.mattermost.homeChannel"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let root_id = string_arg(
        payload,
        &[
            "root_id",
            "rootId",
            "reply_to",
            "replyTo",
            "message_id",
            "messageId",
        ],
    )
    .or(target_root_id)
    .or_else(|| mattermost_home_thread_id(store).ok().flatten());
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "channel_id": channel_id.clone(),
                "message": chunk,
                "root_id": root_id.clone(),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_mattermost_send_message_target(target: &str) -> Option<(Option<String>, Option<String>)> {
    let target = target.trim();
    let rest = target
        .strip_prefix("mattermost:")
        .or_else(|| target.strip_prefix("Mattermost:"))?;
    let resolved =
        resolve_channel_directory_target("mattermost", rest).unwrap_or_else(|| rest.into());
    let mut parts = resolved.splitn(2, ':');
    let channel_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let root_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some((channel_id, root_id))
}

pub(super) fn telegram_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let (target_chat_id, target_thread_id) =
        string_arg(payload, &["target", "targetId", "target_id"])
            .and_then(|target| parse_telegram_send_message_target(&target))
            .unwrap_or((None, None));
    let chat_id = string_arg(payload, &["chat_id", "chatId", "channel_id", "channelId"])
        .or(target_chat_id)
        .or_else(|| telegram_home_channel_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Telegram requires payload.chat_id, target \"telegram:<chat_id>\", or settings.telegram.homeChannel"
                    .into(),
            )
        })?;
    let raw_message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let force_document = payload
        .get("force_document")
        .or_else(|| payload.get("forceDocument"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| raw_message.contains("[[as_document]]"));
    let (message, extracted_media_files) = extract_send_message_media(&raw_message);
    let thread_id = string_arg(
        payload,
        &[
            "thread_id",
            "threadId",
            "message_thread_id",
            "messageThreadId",
        ],
    )
    .or(target_thread_id)
    .or_else(|| telegram_home_thread_id(store).ok().flatten());
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 4_096);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "chat_id": chat_id.clone(),
                "message": chunk,
                "thread_id": thread_id.clone(),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
                "force_document": force_document,
            })
        })
        .collect())
}

fn parse_telegram_send_message_target(target: &str) -> Option<(Option<String>, Option<String>)> {
    let target = target.trim();
    let rest = target
        .strip_prefix("telegram:")
        .or_else(|| target.strip_prefix("Telegram:"))?;
    let resolved =
        resolve_channel_directory_target("telegram", rest).unwrap_or_else(|| rest.into());
    let rest = resolved.as_str();
    let mut parts = rest.splitn(2, ':');
    let chat_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let thread_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some((chat_id, thread_id))
}

fn send_message_targets_yuanbao(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("yuanbao"))
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "yuanbao" || target.starts_with("yuanbao:")
            })
            .unwrap_or(false)
}

fn yuanbao_send_message_payloads(payload: &Value) -> AppResult<Vec<Value>> {
    let target = string_arg(payload, &["target", "targetId", "target_id"]).unwrap_or_default();
    let direct_id = string_arg(payload, &["user_id", "userId", "account_id", "accountId"])
        .or_else(|| parse_yuanbao_direct_target(&target));
    if target.to_ascii_lowercase().starts_with("yuanbao:group:")
        || target.to_ascii_lowercase().starts_with("group:")
    {
        return Err(AppError::BadRequest(
            "send_message Yuanbao group targets require settings.messagingGateway with platform \"yuanbao\" enabled; Hermes exposes Yuanbao group text delivery through the platform adapter, while SynthChat routes that adapter path through the configured messaging gateway. Use yb_query_group_info/yb_query_group_members/yb_send_sticker for dedicated Yuanbao bridge tools."
                .into(),
        ));
    }
    let user_id = direct_id.ok_or_else(|| {
        AppError::BadRequest(
            "send_message to Yuanbao requires target \"yuanbao:direct:<account_id>\" or payload.user_id"
                .into(),
        )
    })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 2_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "user_id": user_id.clone(),
                "message": chunk,
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_yuanbao_direct_target(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .strip_prefix("yuanbao:")
        .or_else(|| target.strip_prefix("Yuanbao:"))
        .unwrap_or(target);
    rest.strip_prefix("direct:")
        .or_else(|| rest.strip_prefix("Direct:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn send_message_targets_feishu(payload: &Value) -> bool {
    payload
        .get("platform")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "feishu" || value == "lark"
        })
        .unwrap_or(false)
        || string_arg(payload, &["target", "targetId", "target_id"])
            .map(|target| {
                let target = target.to_ascii_lowercase();
                target == "feishu" || target == "lark" || target.starts_with("feishu:")
            })
            .unwrap_or(false)
}

pub(super) fn feishu_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let (target_receive_id, target_thread_id) =
        string_arg(payload, &["target", "targetId", "target_id"])
            .and_then(|target| parse_feishu_send_message_target(&target))
            .unwrap_or((None, None));
    let receive_id = string_arg(payload, &["receive_id", "receiveId", "chat_id", "chatId"])
        .or(target_receive_id)
        .or_else(|| feishu_home_channel_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Feishu requires payload.receive_id/chat_id, target \"feishu:<receive_id>\", or settings.feishu.homeChannel"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let thread_id = string_arg(
        payload,
        &[
            "thread_id",
            "threadId",
            "message_id",
            "messageId",
            "reply_to",
            "replyTo",
        ],
    )
    .or(target_thread_id)
    .or_else(|| feishu_home_thread_id(store).ok().flatten());
    let receive_id_type = string_arg(payload, &["receive_id_type", "receiveIdType"]);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 8_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "receive_id": receive_id.clone(),
                "receive_id_type": receive_id_type.clone(),
                "message": chunk,
                "thread_id": thread_id.clone(),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
            })
        })
        .collect())
}

fn parse_feishu_send_message_target(target: &str) -> Option<(Option<String>, Option<String>)> {
    let target = target.trim();
    let rest = target
        .strip_prefix("feishu:")
        .or_else(|| target.strip_prefix("Feishu:"))
        .or_else(|| target.strip_prefix("lark:"))
        .or_else(|| target.strip_prefix("Lark:"))?;
    let resolved = resolve_channel_directory_target("feishu", rest).unwrap_or_else(|| rest.into());
    let rest = resolved.as_str();
    let mut parts = rest.splitn(2, ':');
    let receive_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let thread_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some((receive_id, thread_id))
}

fn feishu_home_channel_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.feishu;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "chatId",
            "chat_id",
            "receiveId",
            "receive_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &[
                "chat_id",
                "chatId",
                "receive_id",
                "receiveId",
                "channel_id",
                "channelId",
            ],
        )
    })
    .or_else(|| env::var("FEISHU_HOME_CHANNEL").ok()))
}

fn feishu_home_thread_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.feishu;
    Ok(home_thread_id_from_config(
        &config,
        &["FEISHU_HOME_CHANNEL_THREAD_ID"],
    ))
}

pub(super) fn discord_send_message_payloads(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let explicit_forum = bool_arg(
        payload,
        &[
            "forum",
            "forumChannel",
            "forum_channel",
            "createForumThread",
            "create_forum_thread",
        ],
    )
    .unwrap_or(false);
    let directory_target =
        string_arg(payload, &["target", "targetId", "target_id"]).and_then(|target| {
            target
                .strip_prefix("discord:")
                .or_else(|| target.strip_prefix("Discord:"))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| resolve_discord_channel_directory_target(value))
        });
    let channel_id = string_arg(payload, &["channel_id", "channelId"])
        .or_else(|| {
            directory_target
                .as_ref()
                .map(|target| target.id.clone())
        })
        .or_else(|| discord_home_channel_id(store).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "send_message to Discord requires payload.channel_id, target \"discord:<channel_id>\", or settings.discord.homeChannel"
                    .into(),
            )
        })?;
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    let (message, extracted_media_files) = extract_send_message_media(&message);
    let media_files = send_message_media_files_value(payload, extracted_media_files);
    let chunks = chunk_message_text(&message, 2_000);
    let last_index = chunks.len().saturating_sub(1);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            json!({
                "action": "send_message",
                "channel_id": channel_id.clone(),
                "content": chunk,
                "tts": payload.get("tts").and_then(Value::as_bool).unwrap_or(false),
                "message_id": string_arg(payload, &["reply_to", "replyTo", "message_id", "messageId"]),
                "media_files": if index == last_index { media_files.clone() } else { json!([]) },
                "forum": explicit_forum || directory_target.as_ref().map(|target| target.is_forum).unwrap_or(false),
                "forum_thread_name": string_arg(payload, &["forum_thread_name", "forumThreadName", "thread_name", "threadName", "name"]),
            })
        })
        .collect())
}

fn bool_arg(payload: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_bool))
}

pub(super) fn extract_send_message_media(message: &str) -> (String, Vec<Value>) {
    let audio_as_voice = message.contains("[[audio_as_voice]]");
    let chars = message.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut media_files = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if starts_with_chars(&chars, index, "[[audio_as_voice]]") {
            index += "[[audio_as_voice]]".chars().count();
            continue;
        }
        if starts_with_chars(&chars, index, "[[as_document]]") {
            index += "[[as_document]]".chars().count();
            continue;
        }
        if starts_with_chars(&chars, index, "MEDIA:") {
            index += "MEDIA:".chars().count();
            let path = if matches!(chars.get(index), Some('"') | Some('\'')) {
                let quote = chars[index];
                index += 1;
                let mut value = String::new();
                while index < chars.len() && chars[index] != quote {
                    value.push(chars[index]);
                    index += 1;
                }
                if index < chars.len() && chars[index] == quote {
                    index += 1;
                }
                value
            } else {
                let mut value = String::new();
                while index < chars.len() && !chars[index].is_whitespace() {
                    value.push(chars[index]);
                    index += 1;
                }
                value
            };
            let path = path.trim();
            if !path.is_empty() {
                media_files.push(json!({
                    "path": path,
                    "is_voice": audio_as_voice,
                    "voice_compatible": audio_as_voice,
                }));
            }
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    (output.trim().to_string(), media_files)
}

fn send_message_media_files_value(payload: &Value, extracted_media_files: Vec<Value>) -> Value {
    let value = payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
        .or_else(|| payload.get("attachments"))
        .or_else(|| payload.get("files"))
        .cloned()
        .unwrap_or_else(|| Value::Array(extracted_media_files));
    normalize_send_message_media_files(value)
}

fn normalize_send_message_media_files(value: Value) -> Value {
    let Some(files) = value.as_array() else {
        return value;
    };
    Value::Array(
        files
            .iter()
            .map(|file| {
                if file.as_str().is_some() {
                    return file.clone();
                }
                let Some(object) = file.as_object() else {
                    return file.clone();
                };
                let mut normalized = object.clone();
                let is_voice = object
                    .get("is_voice")
                    .or_else(|| object.get("isVoice"))
                    .or_else(|| object.get("voice_compatible"))
                    .or_else(|| object.get("voiceCompatible"))
                    .or_else(|| object.get("as_voice"))
                    .or_else(|| object.get("asVoice"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_voice {
                    normalized.insert("is_voice".into(), json!(true));
                    normalized.insert("isVoice".into(), json!(true));
                    normalized.insert("voice_compatible".into(), json!(true));
                    normalized.insert("voiceCompatible".into(), json!(true));
                }
                Value::Object(normalized)
            })
            .collect(),
    )
}

fn starts_with_chars(chars: &[char], index: usize, needle: &str) -> bool {
    let needle = needle.chars().collect::<Vec<_>>();
    chars
        .get(index..index.saturating_add(needle.len()))
        .map(|slice| slice == needle.as_slice())
        .unwrap_or(false)
}

pub(super) fn chunk_message_text(message: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 || message.chars().count() <= max_chars {
        return vec![message.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in message.split_inclusive('\n') {
        if line.chars().count() > max_chars {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut part = String::new();
            for ch in line.chars() {
                part.push(ch);
                if part.chars().count() >= max_chars {
                    chunks.push(std::mem::take(&mut part));
                }
            }
            if !part.is_empty() {
                current.push_str(&part);
            }
            continue;
        }
        if current.chars().count() + line.chars().count() > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn discord_home_channel_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.discord;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "channelId",
            "channel_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("DISCORD_HOME_CHANNEL").ok()))
}

fn telegram_home_channel_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.telegram;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("TELEGRAM_HOME_CHANNEL").ok()))
}

fn telegram_home_thread_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.telegram;
    Ok(home_thread_id_from_config(
        &config,
        &["TELEGRAM_HOME_CHANNEL_THREAD_ID"],
    ))
}

fn slack_home_channel_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.slack;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "channelId",
            "channel_id",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("SLACK_HOME_CHANNEL").ok()))
}

fn slack_home_thread_ts(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.slack;
    Ok(home_thread_id_from_config(
        &config,
        &["SLACK_HOME_CHANNEL_THREAD_ID"],
    ))
}

fn mattermost_home_channel_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.mattermost;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "channelId",
            "channel_id",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("MATTERMOST_HOME_CHANNEL").ok()))
}

fn mattermost_home_thread_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.mattermost;
    Ok(home_thread_id_from_config(
        &config,
        &["MATTERMOST_HOME_CHANNEL_THREAD_ID"],
    ))
}

fn home_thread_id_from_config(config: &Value, env_keys: &[&str]) -> Option<String> {
    string_arg(
        config,
        &[
            "homeThreadId",
            "home_thread_id",
            "homeChannelThreadId",
            "home_channel_thread_id",
            "threadId",
            "thread_id",
            "messageThreadId",
            "message_thread_id",
            "threadTs",
            "thread_ts",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            config,
            &[
                "thread_id",
                "threadId",
                "message_thread_id",
                "messageThreadId",
                "thread_ts",
                "threadTs",
            ],
        )
    })
    .or_else(|| {
        env_keys
            .iter()
            .find_map(|key| env::var(key).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn home_channel_field_from_config(config: &Value, field_keys: &[&str]) -> Option<String> {
    for object_key in ["home_channel", "homeChannel", "home"] {
        let Some(home_channel) = config.get(object_key) else {
            continue;
        };
        if let Some(value) = string_arg(home_channel, field_keys) {
            return Some(value);
        }
    }
    None
}

fn matrix_home_room_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.matrix;
    Ok(string_arg(
        &config,
        &[
            "homeRoom",
            "home_room",
            "homeRoomId",
            "home_room_id",
            "roomId",
            "room_id",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "room_id", "roomId"])
    })
    .or_else(|| env::var("MATRIX_HOME_ROOM").ok()))
}

fn signal_home_recipient_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.signal;
    Ok(string_arg(
        &config,
        &[
            "homeRecipient",
            "home_recipient",
            "homeRecipientId",
            "home_recipient_id",
            "recipient",
            "recipientId",
            "recipient_id",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &[
                "chat_id",
                "chatId",
                "recipient",
                "recipient_id",
                "recipientId",
            ],
        )
    })
    .or_else(|| env::var("SIGNAL_HOME_CHANNEL").ok())
    .or_else(|| env::var("SIGNAL_HOME_RECIPIENT").ok()))
}

fn email_home_address(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.email;
    Ok(string_arg(
        &config,
        &["homeAddress", "home_address", "homeEmail", "home_email"],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &["chat_id", "chatId", "address", "email", "to", "recipient"],
        )
    })
    .or_else(|| string_arg(&config, &["to", "recipient"]))
    .or_else(|| env::var("EMAIL_HOME_ADDRESS").ok()))
}

fn sms_home_number(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.sms;
    Ok(string_arg(
        &config,
        &[
            "homeNumber",
            "home_number",
            "homePhone",
            "home_phone",
            "to",
            "number",
            "recipient",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &["chat_id", "chatId", "number", "phone", "to", "recipient"],
        )
    })
    .or_else(|| env::var("SMS_HOME_CHANNEL").ok())
    .or_else(|| env::var("SMS_HOME_NUMBER").ok()))
}

fn dingtalk_home_target(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.dingtalk;
    Ok(string_arg(
        &config,
        &["homeTarget", "home_target", "target", "chatId", "chat_id"],
    )
    .or_else(|| home_channel_field_from_config(&config, &["chat_id", "chatId", "target"]))
    .or_else(|| env::var("DINGTALK_HOME_CHANNEL").ok())
    .or_else(|| env::var("DINGTALK_HOME_TARGET").ok()))
}

fn teams_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.teams;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("TEAMS_HOME_CHANNEL").ok())
    .or_else(|| env::var("TEAMS_CHAT_ID").ok())
    .or_else(|| env::var("TEAMS_CHANNEL_ID").ok()))
}

fn ntfy_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.ntfy;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "publishTopic",
            "publish_topic",
            "topic",
        ],
    )
    .or_else(|| home_channel_field_from_config(&config, &["topic", "chat_id", "chatId"]))
    .or_else(|| env::var("NTFY_HOME_CHANNEL").ok())
    .or_else(|| env::var("NTFY_PUBLISH_TOPIC").ok())
    .or_else(|| env::var("NTFY_TOPIC").ok()))
}

fn simplex_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.simplex;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("SIMPLEX_HOME_CHANNEL").ok()))
}

fn irc_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.irc;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "channel",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| home_channel_field_from_config(&config, &["channel", "chat_id", "chatId"]))
    .or_else(|| env::var("IRC_HOME_CHANNEL").ok())
    .or_else(|| env::var("IRC_CHANNEL").ok()))
}

fn line_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.line;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
        ],
    )
    .or_else(|| home_channel_field_from_config(&config, &["chat_id", "chatId"]))
    .or_else(|| env::var("LINE_HOME_CHANNEL").ok()))
}

fn google_chat_home_channel(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.google_chat;
    Ok(string_arg(
        &config,
        &[
            "homeChannel",
            "home_channel",
            "homeTarget",
            "chatId",
            "chat_id",
            "space",
            "spaceName",
        ],
    )
    .or_else(|| home_channel_field_from_config(&config, &["chat_id", "chatId", "space"]))
    .or_else(|| env::var("GOOGLE_CHAT_HOME_CHANNEL").ok()))
}

fn whatsapp_home_chat_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.whatsapp;
    Ok(string_arg(
        &config,
        &[
            "homeChatId",
            "home_chat_id",
            "homeChannel",
            "home_channel",
            "chatId",
            "chat_id",
            "to",
            "recipient",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("WHATSAPP_HOME_CHANNEL").ok())
    .or_else(|| env::var("WHATSAPP_HOME_CHAT_ID").ok()))
}

fn qqbot_home_target(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.qqbot;
    Ok(string_arg(
        &config,
        &[
            "homeTarget",
            "home_target",
            "homeChannel",
            "home_channel",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
            "to",
            "recipient",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(&config, &["chat_id", "chatId", "channel_id", "channelId"])
    })
    .or_else(|| env::var("QQBOT_HOME_CHANNEL").ok())
    .or_else(|| env::var("QQ_HOME_CHANNEL").ok())
    .or_else(|| env::var("QQBOT_HOME_TARGET").ok()))
}

fn homeassistant_home_notify_target(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.homeassistant;
    Ok(string_arg(
        &config,
        &[
            "homeNotifyTarget",
            "home_notify_target",
            "notifyTarget",
            "notify_target",
            "target",
            "chatId",
            "chat_id",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &[
                "chat_id",
                "chatId",
                "target",
                "notify_target",
                "notifyTarget",
            ],
        )
    })
    .or_else(|| env::var("HASS_HOME_NOTIFY_TARGET").ok()))
}

fn bluebubbles_home_chat_id(store: &AppStore) -> AppResult<Option<String>> {
    let config = store.config()?.bluebubbles;
    Ok(string_arg(
        &config,
        &[
            "homeChatId",
            "home_chat_id",
            "homeTarget",
            "home_target",
            "chatId",
            "chat_id",
            "address",
            "to",
            "recipient",
        ],
    )
    .or_else(|| {
        home_channel_field_from_config(
            &config,
            &["chat_id", "chatId", "address", "to", "recipient"],
        )
    })
    .or_else(|| env::var("BLUEBUBBLES_HOME_CHANNEL").ok())
    .or_else(|| env::var("BLUEBUBBLES_HOME_CHAT_ID").ok()))
}

fn channel_directory_path() -> Option<PathBuf> {
    for key in [
        "SYNTHCHAT_CHANNEL_DIRECTORY_PATH",
        "HERMES_CHANNEL_DIRECTORY_PATH",
    ] {
        if let Ok(value) = env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    env::var("HERMES_HOME")
        .ok()
        .map(|home| PathBuf::from(home).join("channel_directory.json"))
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .map(|home| PathBuf::from(home).join(".hermes/channel_directory.json"))
        })
        .or_else(|| {
            env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".hermes/channel_directory.json"))
        })
}

fn load_channel_directory() -> Option<Value> {
    let path = channel_directory_path()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}

pub(super) fn channel_directory_status_snapshot(store: &AppStore) -> Value {
    let path = channel_directory_path();
    let directory = load_channel_directory();
    let (platform_count, target_count) = directory
        .as_ref()
        .map(channel_directory_counts)
        .unwrap_or((0, 0));
    let platforms = directory
        .as_ref()
        .and_then(|value| value.get("platforms"))
        .and_then(Value::as_object)
        .map(|platforms| {
            platforms
                .iter()
                .map(|(platform, channels)| {
                    (
                        platform.clone(),
                        json!({
                            "targetCount": channels.as_array().map(Vec::len).unwrap_or(0),
                            "supportsNameResolution": true,
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    let mut session_platforms: BTreeMap<String, usize> = BTreeMap::new();
    let mut origin_conversation_count = 0_usize;
    for conversation in store.conversations().unwrap_or_default() {
        let origin = conversation
            .metadata
            .get("origin")
            .or_else(|| conversation.metadata.get("source"))
            .unwrap_or(&conversation.metadata);
        let Some(platform) = delivery_mirror_metadata_string(origin, &["platform"])
            .or_else(|| delivery_mirror_metadata_string(&conversation.metadata, &["platform"]))
        else {
            continue;
        };
        let has_chat = delivery_mirror_metadata_string(
            origin,
            &[
                "chatId",
                "chat_id",
                "channelId",
                "channel_id",
                "roomId",
                "room_id",
            ],
        )
        .or_else(|| {
            delivery_mirror_metadata_string(
                &conversation.metadata,
                &[
                    "chatId",
                    "chat_id",
                    "channelId",
                    "channel_id",
                    "roomId",
                    "room_id",
                ],
            )
        })
        .is_some();
        if has_chat {
            origin_conversation_count += 1;
            *session_platforms
                .entry(platform.to_ascii_lowercase())
                .or_default() += 1;
        }
    }
    json!({
        "schema": "hermes_channel_directory_desktop_v1",
        "path": path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        "exists": path.as_ref().map(|path| path.exists()).unwrap_or(false),
        "updatedAt": directory
            .as_ref()
            .and_then(|value| value.get("updated_at").or_else(|| value.get("updatedAt")))
            .cloned()
            .unwrap_or(Value::Null),
        "platformCount": platform_count,
        "targetCount": target_count,
        "platforms": platforms,
        "sessionDiscovery": {
            "source": "conversation.metadata.origin",
            "originConversationCount": origin_conversation_count,
            "platforms": session_platforms,
            "matchesHermesSessionStore": true
        },
        "deliveryRouting": {
            "targetFormats": [
                "origin",
                "local",
                "platform",
                "platform:chat_id",
                "platform:chat_id:thread_id"
            ],
            "nameResolution": directory.is_some(),
            "originMirror": true,
            "mirrorMatchKeys": ["platform", "chat_id/channel_id/room_id", "thread_id", "user_id"],
            "silenceNarrationFilter": {
                "enabled": send_message_silence_narration_filter_enabled(),
                "scope": "platform_outbound",
                "localDeliveryFiltered": false,
                "filteredReason": "hermes_delivery_silence_narration"
            }
        }
    })
}

fn send_message_channel_directory_from_payload(payload: &Value) -> AppResult<Value> {
    let directory = if let Some(directory) = payload.get("directory") {
        directory.clone()
    } else if let Some(platforms) = payload.get("platforms") {
        json!({
            "updated_at": payload.get("updated_at").or_else(|| payload.get("updatedAt")).cloned().unwrap_or(Value::Null),
            "platforms": platforms.clone(),
        })
    } else {
        return Err(AppError::BadRequest(
            "send_message import_directory requires a directory object or platforms object".into(),
        ));
    };
    validate_channel_directory(&directory)?;
    Ok(directory)
}

fn validate_channel_directory(directory: &Value) -> AppResult<()> {
    let Some(platforms) = directory.get("platforms").and_then(Value::as_object) else {
        return Err(AppError::BadRequest(
            "channel directory must contain a platforms object".into(),
        ));
    };
    for (platform, channels) in platforms {
        if !channels.is_array() {
            return Err(AppError::BadRequest(format!(
                "channel directory platform '{platform}' must be an array"
            )));
        }
    }
    Ok(())
}

fn channel_directory_counts(directory: &Value) -> (usize, usize) {
    let Some(platforms) = directory.get("platforms").and_then(Value::as_object) else {
        return (0, 0);
    };
    let target_count = platforms
        .values()
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum();
    (platforms.len(), target_count)
}

fn write_channel_directory(directory: &Value) -> AppResult<PathBuf> {
    validate_channel_directory(directory)?;
    let Some(path) = channel_directory_path() else {
        return Err(AppError::BadRequest(
            "channel directory path is unavailable; set SYNTHCHAT_CHANNEL_DIRECTORY_PATH or HERMES_CHANNEL_DIRECTORY_PATH".into(),
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let tmp_path = path.with_extension(format!(
        "{}.tmp-{suffix}",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));
    fs::write(&tmp_path, serde_json::to_vec_pretty(directory)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&tmp_path, &path)?;
    Ok(path)
}

fn send_message_import_channel_directory(payload: &Value) -> AppResult<String> {
    let directory = send_message_channel_directory_from_payload(payload)?;
    let (platform_count, target_count) = channel_directory_counts(&directory);
    let path = write_channel_directory(&directory)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "action": "import_directory",
        "path": path.display().to_string(),
        "updatedAt": directory.get("updated_at").or_else(|| directory.get("updatedAt")).cloned().unwrap_or(Value::Null),
        "platformCount": platform_count,
        "targetCount": target_count,
    }))?)
}

async fn send_message_refresh_channel_directory(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let url = string_arg(payload, &["url", "directoryUrl", "directory_url"])
        .or_else(|| env::var("SYNTHCHAT_CHANNEL_DIRECTORY_URL").ok())
        .or_else(|| env::var("HERMES_CHANNEL_DIRECTORY_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if url.is_none() && send_message_refreshes_mattermost_directory(store, payload)? {
        let config = store.config()?.mattermost;
        let directory = mattermost_channel_directory(&config).await?;
        validate_channel_directory(&directory)?;
        let (platform_count, target_count) = channel_directory_counts(&directory);
        let path = write_channel_directory(&directory)?;
        return Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "action": "refresh_directory",
            "source": "mattermost",
            "path": path.display().to_string(),
            "updatedAt": directory.get("updated_at").or_else(|| directory.get("updatedAt")).cloned().unwrap_or(Value::Null),
            "platformCount": platform_count,
            "targetCount": target_count,
        }))?);
    }
    let url = url.ok_or_else(|| {
        AppError::BadRequest(
            "send_message refresh_directory requires url, SYNTHCHAT_CHANNEL_DIRECTORY_URL, or configured platform source such as Mattermost"
                .into(),
        )
    })?;
    let token = string_arg(payload, &["token", "bearerToken", "bearer_token"])
        .or_else(|| env::var("SYNTHCHAT_CHANNEL_DIRECTORY_TOKEN").ok())
        .or_else(|| env::var("HERMES_CHANNEL_DIRECTORY_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .map_err(|err| AppError::BadRequest(format!("failed to create HTTP client: {err}")))?;
    let mut request = client.get(&url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|err| AppError::BadRequest(format!("failed to refresh channel directory: {err}")))?
        .error_for_status()
        .map_err(|err| {
            AppError::BadRequest(format!("failed to refresh channel directory: {err}"))
        })?;
    let directory = response.json::<Value>().await.map_err(|err| {
        AppError::BadRequest(format!("invalid channel directory response: {err}"))
    })?;
    validate_channel_directory(&directory)?;
    let (platform_count, target_count) = channel_directory_counts(&directory);
    let path = write_channel_directory(&directory)?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "action": "refresh_directory",
        "url": url,
        "path": path.display().to_string(),
        "updatedAt": directory.get("updated_at").or_else(|| directory.get("updatedAt")).cloned().unwrap_or(Value::Null),
        "platformCount": platform_count,
        "targetCount": target_count,
    }))?)
}

fn send_message_refreshes_mattermost_directory(
    store: &AppStore,
    payload: &Value,
) -> AppResult<bool> {
    let platform = string_arg(payload, &["platform", "source", "provider"])
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if platform.as_deref() == Some("mattermost") {
        return Ok(true);
    }
    Ok(platform.is_none() && mattermost_configured(&store.config()?.mattermost))
}

fn normalize_channel_query(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_ascii_lowercase()
}

fn channel_target_name(platform: &str, channel: &Value) -> Option<String> {
    let name = channel_directory_channel_name(channel)?;
    if platform == "discord" && channel.get("guild").and_then(Value::as_str).is_some() {
        return Some(format!("#{name}"));
    }
    if platform != "discord" {
        if let Some(kind) = channel
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Some(format!("{name} ({kind})"));
        }
    }
    Some(name.to_string())
}

fn channel_directory_channel_name(channel: &Value) -> Option<String> {
    string_arg(
        channel,
        &[
            "name",
            "display_name",
            "displayName",
            "title",
            "handle",
            "chat_name",
            "chatName",
            "user_name",
            "userName",
        ],
    )
}

fn channel_directory_match_names(platform: &str, channel: &Value) -> Vec<String> {
    let mut names = Vec::new();
    for key in [
        "name",
        "display_name",
        "displayName",
        "title",
        "handle",
        "chat_name",
        "chatName",
        "user_name",
        "userName",
    ] {
        if let Some(value) = channel
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            names.push(value.to_string());
        }
    }
    for key in ["aliases", "alias"] {
        match channel.get(key) {
            Some(Value::Array(values)) => {
                names.extend(values.iter().filter_map(|value| {
                    value
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                }));
            }
            Some(Value::String(value)) => {
                let value = value.trim();
                if !value.is_empty() {
                    names.push(value.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(target_name) = channel_target_name(platform, channel) {
        names.push(target_name);
    }
    names.sort();
    names.dedup();
    names
}

fn resolve_channel_directory_target(platform: &str, name: &str) -> Option<String> {
    resolve_channel_directory_entry(platform, name)
        .and_then(|entry| entry.get("id").and_then(Value::as_str).map(str::to_string))
}

#[derive(Clone, Debug)]
struct DiscordDirectoryTarget {
    id: String,
    is_forum: bool,
}

fn resolve_discord_channel_directory_target(name: &str) -> DiscordDirectoryTarget {
    if let Some(entry) = resolve_channel_directory_entry("discord", name) {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            return DiscordDirectoryTarget {
                id: id.to_string(),
                is_forum: kind == "forum" || kind == "guild_forum",
            };
        }
    }
    DiscordDirectoryTarget {
        id: name.to_string(),
        is_forum: false,
    }
}

fn resolve_channel_directory_entry(platform: &str, name: &str) -> Option<Value> {
    let directory = load_channel_directory()?;
    let channels = directory
        .get("platforms")
        .and_then(|platforms| platforms.get(platform))
        .and_then(Value::as_array)?;
    if channels.is_empty() {
        return None;
    }
    let raw = name.trim();
    for channel in channels {
        if channel.get("id").and_then(Value::as_str) == Some(raw) {
            return Some(channel.clone());
        }
    }
    let query = normalize_channel_query(raw);
    for channel in channels {
        if channel_directory_match_names(platform, channel)
            .iter()
            .any(|name| normalize_channel_query(name) == query)
        {
            return Some(channel.clone());
        }
    }
    if platform == "discord" && query.contains('/') {
        let mut parts = query.rsplitn(2, '/');
        let channel_part = parts.next().unwrap_or_default();
        let guild_part = parts.next().unwrap_or_default();
        for channel in channels {
            let guild = channel
                .get("guild")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_ascii_lowercase();
            if guild == guild_part
                && channel_directory_match_names(platform, channel)
                    .iter()
                    .any(|name| normalize_channel_query(name) == channel_part)
            {
                return Some(channel.clone());
            }
        }
    }
    let matches = channels
        .iter()
        .filter(|channel| {
            channel_directory_match_names(platform, channel)
                .iter()
                .any(|value| normalize_channel_query(value).starts_with(&query))
        })
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        return Some(matches[0].clone());
    }
    None
}

fn send_message_channel_directory_targets() -> Vec<Value> {
    let Some(directory) = load_channel_directory() else {
        return Vec::new();
    };
    let updated_at = directory.get("updated_at").cloned().unwrap_or(Value::Null);
    let Some(platforms) = directory.get("platforms").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for (platform, channels) in platforms {
        let Some(channels) = channels.as_array() else {
            continue;
        };
        for channel in channels {
            let Some(id) = channel.get("id").and_then(Value::as_str) else {
                continue;
            };
            let target_name = channel_target_name(platform, channel)
                .or_else(|| {
                    channel
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| id.to_string());
            targets.push(json!({
                "platform": platform,
                "target": format!("{platform}:{target_name}"),
                "resolvedTarget": format!("{platform}:{id}"),
                "id": id,
                "name": channel.get("name").cloned().unwrap_or(Value::Null),
                "displayName": channel.get("display_name").or_else(|| channel.get("displayName")).cloned().unwrap_or(Value::Null),
                "aliases": channel.get("aliases").cloned().unwrap_or(Value::Null),
                "type": channel.get("type").cloned().unwrap_or(Value::Null),
                "guild": channel.get("guild").cloned().unwrap_or(Value::Null),
                "threadId": channel.get("thread_id").or_else(|| channel.get("threadId")).cloned().unwrap_or(Value::Null),
                "chatTopic": channel.get("chat_topic").or_else(|| channel.get("chatTopic")).cloned().unwrap_or(Value::Null),
                "updatedAt": updated_at,
                "source": "hermesChannelDirectory",
            }));
        }
    }
    targets
}

fn send_message_list_targets(store: &AppStore) -> AppResult<String> {
    let conversations = store
        .conversations()?
        .into_iter()
        .map(|conversation| {
            json!({
                "id": conversation.id,
                "title": conversation.title,
                "updatedAt": conversation.updated_at,
                "lastMessage": truncate_for_prompt(&conversation.last_message, 200),
                "agentId": conversation.agent_id,
                "personaId": conversation.persona_id,
            })
        })
        .collect::<Vec<_>>();
    let external_targets = send_message_external_targets(store)?;
    let directory_targets = send_message_channel_directory_targets();
    Ok(serde_json::to_string_pretty(&json!({
        "targets": conversations,
        "externalTargets": external_targets,
        "directoryTargets": directory_targets,
        "currentAlias": "current"
    }))?)
}

pub(super) fn send_message_external_targets(store: &AppStore) -> AppResult<Vec<Value>> {
    let config = store.config()?;
    let mut targets = Vec::new();
    if discord_settings(&config.discord).is_ok() {
        targets.push(json!({
            "platform": "discord",
            "target": "discord:<channel_id>",
            "homeTarget": discord_home_channel_id(store)?.map(|channel_id| format!("discord:{channel_id}")),
            "messageKey": "message",
            "notes": "Use target \"discord\" only when settings.discord.homeChannel is configured."
        }));
    }
    if feishu_settings(&config.feishu).is_ok() {
        targets.push(json!({
            "platform": "feishu",
            "target": "feishu:<receive_id>",
            "homeTarget": home_target_with_thread("feishu", feishu_home_channel_id(store)?, feishu_home_thread_id(store)?),
            "messageKey": "message",
            "notes": "Feishu/Lark send_message routing supports text plus MEDIA:<path> image/file uploads; use feishu:<receive_id>:<reply_message_id> to reply."
        }));
    }
    if yuanbao_bridge_available(&config.yuanbao) {
        targets.push(json!({
            "platform": "yuanbao",
            "target": "yuanbao:direct:<account_id>",
            "messageKey": "message",
            "notes": "Direct Yuanbao send_message routing maps to yb_send_dm with user_id. Yuanbao group text targets route through the Hermes-compatible messaging gateway when settings.messagingGateway enables platform yuanbao; dedicated Yuanbao bridge tools cover group/member lookup, DM, and stickers."
        }));
    }
    if telegram_configured(&config.telegram) {
        targets.push(json!({
            "platform": "telegram",
            "target": "telegram:<chat_id>",
            "homeTarget": home_target_with_thread("telegram", telegram_home_channel_id(store)?, telegram_home_thread_id(store)?),
            "messageKey": "message",
            "notes": "Telegram Bot API send_message routing supports telegram:<chat_id>:<message_thread_id> for topics, MEDIA:<path> photo/video/voice/audio/document uploads, and [[as_document]] to force document delivery."
        }));
    }
    if slack_configured(&config.slack) {
        targets.push(json!({
            "platform": "slack",
            "target": "slack:<channel_id>",
            "homeTarget": home_target_with_thread("slack", slack_home_channel_id(store)?, slack_home_thread_ts(store)?),
            "messageKey": "message",
            "notes": "Slack routing supports slack:<channel_id>:<thread_ts>, Slack user DM targets, text via chat.postMessage, and local MEDIA attachments via files.getUploadURLExternal/files.completeUploadExternal."
        }));
    }
    if mattermost_configured(&config.mattermost) {
        targets.push(json!({
            "platform": "mattermost",
            "target": "mattermost:<channel_id>",
            "homeTarget": home_target_with_thread("mattermost", mattermost_home_channel_id(store)?, mattermost_home_thread_id(store)?),
            "messageKey": "message",
            "notes": "Mattermost REST routing supports mattermost:<channel_id>:<root_id> thread replies, text posts, and MEDIA:<path> local file uploads."
        }));
    }
    if matrix_configured(&config.matrix) {
        targets.push(json!({
            "platform": "matrix",
            "target": "matrix:<room_id>",
            "homeTarget": matrix_home_room_id(store)?.map(|room_id| format!("matrix:{room_id}")),
            "messageKey": "message",
            "notes": "Matrix Client-Server API routing supports matrix:<room_id> text plus MEDIA:<path> uploads for unencrypted rooms."
        }));
    }
    if signal_configured(&config.signal) {
        targets.push(json!({
            "platform": "signal",
            "target": "signal:<recipient>",
            "homeTarget": signal_home_recipient_id(store)?.map(|recipient| format!("signal:{recipient}")),
            "messageKey": "message",
            "notes": "Signal signal-cli JSON-RPC routing supports E.164 recipients and signal:group:<group_id> plus MEDIA:<path> attachments, batched at 32 attachments per message."
        }));
    }
    if email_configured(&config.email) {
        targets.push(json!({
            "platform": "email",
            "target": "email:<address>",
            "homeTarget": email_home_address(store)?.map(|address| format!("email:{address}")),
            "messageKey": "message",
            "notes": "Email SMTP routing uses settings.email or EMAIL_ADDRESS, EMAIL_PASSWORD, EMAIL_SMTP_HOST and optional EMAIL_SMTP_PORT. Local MEDIA attachments are sent as multipart/mixed attachments."
        }));
    }
    if sms_configured(&config.sms) {
        targets.push(json!({
            "platform": "sms",
            "target": "sms:<phone>",
            "homeTarget": sms_home_number(store)?.map(|number| format!("sms:{number}")),
            "messageKey": "message",
            "notes": "SMS routing uses Twilio settings.sms or TWILIO_ACCOUNT_SID, TWILIO_AUTH_TOKEN, TWILIO_PHONE_NUMBER. MEDIA:https://... public URLs are sent as Twilio MediaUrl fields; local file uploads are not supported by this route."
        }));
    }
    if dingtalk_configured(&config.dingtalk) {
        targets.push(json!({
            "platform": "dingtalk",
            "target": "dingtalk:<target>",
            "homeTarget": dingtalk_home_target(store)?.map(|target| format!("dingtalk:{target}")),
            "messageKey": "message",
            "notes": "DingTalk robot webhook routing uses settings.dingtalk.webhookUrl or DINGTALK_WEBHOOK_URL. MEDIA:https://... remote image URLs are rendered as markdown images; local file uploads are not supported by session webhook routing."
        }));
    }
    if teams_configured(&config.teams) {
        targets.push(json!({
            "platform": "teams",
            "target": "teams:<chat_id>",
            "homeTarget": teams_home_channel(store)?.map(|target| format!("teams:{target}")),
            "messageKey": "message",
            "notes": "Microsoft Teams routing adapts the Hermes Teams plugin standalone delivery: incoming webhook text posts, Microsoft Graph chat/channel message creation, and Bot Framework message/typing activity POST with validated serviceUrl conversation targets. Live Teams SDK webhook hosting remains an external Teams SDK runtime boundary."
        }));
    }
    if ntfy_configured(&config.ntfy) {
        targets.push(json!({
            "platform": "ntfy",
            "target": "ntfy:<topic>",
            "homeTarget": ntfy_home_channel(store)?.map(|target| format!("ntfy:{target}")),
            "messageKey": "message",
            "notes": "ntfy routing adapts the Hermes ntfy plugin standalone sender: HTTP POST to a topic, Bearer/Basic auth, X-Tags echo-loop prevention, optional X-Markdown, and 4096-character chunks. Live streaming receive remains an external gateway runtime boundary."
        }));
    }
    if simplex_configured(&config.simplex) {
        targets.push(json!({
            "platform": "simplex",
            "target": "simplex:<contact_id> or simplex:group:<group_id>",
            "homeTarget": simplex_home_channel(store)?.map(|target| format!("simplex:{target}")),
            "messageKey": "message",
            "notes": "SimpleX routing adapts the Hermes SimpleX plugin standalone sender: ephemeral WebSocket command delivery to the SimpleX daemon using @[contact] or #[group] syntax and 16000-character chunks. MEDIA attachments require the live daemon file-transfer flow and are not supported by this send-only route."
        }));
    }
    if irc_configured(&config.irc) {
        targets.push(json!({
            "platform": "irc",
            "target": "irc:<channel_or_nick>",
            "homeTarget": irc_home_channel(store)?.map(|target| format!("irc:{target}")),
            "messageKey": "message",
            "notes": "IRC routing adapts the Hermes IRC plugin standalone sender: ephemeral TCP/TLS registration with a -cron nick, optional PASS/NickServ, JOIN-before-channel-PRIVMSG, PING handling, nickname collision retry, and IRC-safe message splitting. MEDIA attachments are not supported."
        }));
    }
    if line_configured(&config.line) {
        targets.push(json!({
            "platform": "line",
            "target": "line:<user_or_group_or_room_id>",
            "homeTarget": line_home_channel(store)?.map(|target| format!("line:{target}")),
            "messageKey": "message",
            "notes": "LINE routing adapts the Hermes LINE plugin sender and gateway receiver: Push API delivery to user/group/room IDs, Reply API token-first delivery with Push fallback from conversation origin metadata, LINE webhook receive, postback receive, Content API media cache, cached-media public URLs under /media/attachments, markdown-to-text conversion, 4500-character bubbles, and five-message call cap. Slow-response postback button emission and cache retrieval are handled by the gateway."
        }));
    }
    if google_chat_configured(&config.google_chat) {
        targets.push(json!({
            "platform": "google_chat",
            "target": "google_chat:spaces/<id> or google_chat:users/<id>",
            "homeTarget": google_chat_home_channel(store)?.map(|target| format!("google_chat:{target}")),
            "messageKey": "message",
            "notes": "Google Chat routing adapts the Hermes Google Chat plugin standalone sender: strict spaces/<id> or users/<id> resource targets, optional thread_id spaces/<id>/threads/<id>, conversation-origin chat/thread/sender inference for gateway replies, REST API message creation, Bearer access-token delivery, service-account JWT token refresh, ADC authorized-user refresh, 4000-character chunks, native user-OAuth file attachments, /setup-files OAuth token bootstrap through the gateway, Pub/Sub receive, typing markers, and message edits. Hermes' exact gRPC subscriber and Python adapter in-memory user-client cache remain runtime boundaries."
        }));
    }
    if whatsapp_configured(&config.whatsapp) {
        targets.push(json!({
            "platform": "whatsapp",
            "target": "whatsapp:<chat_id>",
            "homeTarget": whatsapp_home_chat_id(store)?.map(|chat_id| format!("whatsapp:{chat_id}")),
            "messageKey": "message",
            "notes": "WhatsApp bridge routing posts text to /send and local MEDIA attachments to /send-media via settings.whatsapp.bridgeUrl or WHATSAPP_BRIDGE_URL."
        }));
    }
    if qqbot_configured(&config.qqbot) {
        targets.push(json!({
            "platform": "qqbot",
            "target": "qqbot:<id>",
            "homeTarget": qqbot_home_target(store)?.map(|target| format!("qqbot:{target}")),
            "messageKey": "message",
            "notes": "QQBot REST routing uses settings.qqbot.appId/clientSecret or QQ_APP_ID/QQ_CLIENT_SECRET. Text tries channel, C2C, then group endpoints; local MEDIA attachments use C2C/group /files upload followed by msg_type 7 media messages."
        }));
    }
    if homeassistant_configured(&config.homeassistant) {
        targets.push(json!({
            "platform": "homeassistant",
            "target": "homeassistant:<notify_target>",
            "homeTarget": homeassistant_home_notify_target(store)?.map(|target| format!("homeassistant:{target}")),
            "messageKey": "message",
            "notes": "Home Assistant notify routing posts to /api/services/notify/notify using settings.homeassistant or HASS_URL/HASS_TOKEN. MEDIA attachments are not supported."
        }));
    }
    if bluebubbles_configured(&config.bluebubbles) {
        targets.push(json!({
            "platform": "bluebubbles",
            "target": "bluebubbles:<chat_id>",
            "homeTarget": bluebubbles_home_chat_id(store)?.map(|chat_id| format!("bluebubbles:{chat_id}")),
            "messageKey": "message",
            "notes": "BlueBubbles routing uses settings.bluebubbles.serverUrl/password or BLUEBUBBLES_SERVER_URL/BLUEBUBBLES_PASSWORD. Text and MEDIA:<path> attachments are supported; raw chat GUIDs and resolvable handles are supported."
        }));
    }
    if messaging_gateway_configured(&config.messaging_gateway) {
        if messaging_gateway_platform_enabled(&config.messaging_gateway, "wecom") {
            targets.push(json!({
                "platform": "wecom",
                "target": "wecom:<chat_id>",
                "messageKey": "message",
                "source": "messagingGateway",
                "notes": "Routes through settings.messagingGateway to a Hermes-compatible WeCom adapter endpoint."
            }));
        }
        if messaging_gateway_platform_enabled(&config.messaging_gateway, "weixin") {
            targets.push(json!({
                "platform": "weixin",
                "target": "weixin:<chat_id>",
                "messageKey": "message",
                "source": "messagingGateway",
                "notes": "Routes through settings.messagingGateway to a Hermes-compatible Weixin/iLink adapter endpoint."
            }));
        }
        if messaging_gateway_platform_enabled(&config.messaging_gateway, "yuanbao") {
            targets.push(json!({
                "platform": "yuanbao",
                "target": "yuanbao:group:<group_code>",
                "messageKey": "message",
                "source": "messagingGateway",
                "notes": "Routes Yuanbao group targets through settings.messagingGateway while direct DMs continue to use the SynthChat Yuanbao bridge."
            }));
        }
    }
    Ok(targets)
}

fn home_target_with_thread(
    platform: &str,
    target_id: Option<String>,
    thread_id: Option<String>,
) -> Option<String> {
    target_id.map(|target_id| {
        if let Some(thread_id) = thread_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            format!("{platform}:{target_id}:{thread_id}")
        } else {
            format!("{platform}:{target_id}")
        }
    })
}

fn send_message_to_local_conversation(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let message = string_arg(payload, &["message", "text", "content", "body"])
        .ok_or_else(|| AppError::BadRequest("send_message requires payload.message".into()))?;
    if message.chars().count() > 20_000 {
        return Err(AppError::BadRequest(
            "send_message message exceeds 20000 characters".into(),
        ));
    }
    let conversation = resolve_send_message_conversation(store, current_conversation_id, payload)?;
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("assistant");
    if !matches!(role, "assistant" | "user" | "tool" | "system") {
        return Err(AppError::BadRequest(format!(
            "unsupported send_message role '{role}'"
        )));
    }
    let source = payload
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("desktop-agent-send-message");
    let saved = store.append_message(ChatMessage::new(
        conversation.id.clone(),
        role,
        message,
        source,
    ))?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "target": {
            "id": conversation.id,
            "title": conversation.title,
        },
        "message": saved
    }))?)
}

fn resolve_send_message_conversation(
    store: &AppStore,
    current_conversation_id: &str,
    payload: &Value,
) -> AppResult<Conversation> {
    let target = string_arg(
        payload,
        &[
            "target",
            "targetId",
            "target_id",
            "conversationId",
            "conversation_id",
            "title",
        ],
    )
    .unwrap_or_else(|| "current".into());
    if target.eq_ignore_ascii_case("current") || target.trim().is_empty() {
        return store.conversation(current_conversation_id);
    }
    if let Ok(conversation) = store.conversation(&target) {
        return Ok(conversation);
    }
    let needle = target.trim().to_lowercase();
    let matches = store
        .conversations()?
        .into_iter()
        .filter(|conversation| conversation.title.trim().to_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(AppError::NotFound(format!(
            "send_message target not found: {target}"
        ))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(AppError::BadRequest(format!(
            "send_message target '{target}' matched multiple conversations; use conversationId"
        ))),
    }
}

pub(super) fn clarify_tool(payload: &Value) -> AppResult<String> {
    let question = string_arg(payload, &["question", "prompt"])
        .ok_or_else(|| AppError::BadRequest("clarify requires payload.question".into()))?;
    let question = question.trim();
    if question.is_empty() {
        return Err(AppError::BadRequest(
            "clarify question cannot be empty".into(),
        ));
    }
    let choices = payload
        .get("choices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|choice| !choice.is_empty())
                .take(4)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty());
    let text = if let Some(choices) = choices.as_ref() {
        format!(
            "Clarification required: {question}\nChoices: {}",
            choices.join(" | ")
        )
    } else {
        format!("Clarification required: {question}")
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": "clarify",
        "requiresUserInput": true,
        "question": question,
        "choices": choices,
        "text": text
    }))?)
}
