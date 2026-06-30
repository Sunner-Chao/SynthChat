use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AppConfig {
    pub log_level: String,
    pub chat: ChatConfig,
    pub reply: Value,
    pub web: Value,
    pub weather: Value,
    pub homeassistant: Value,
    pub feishu: Value,
    pub yuanbao: Value,
    pub telegram: Value,
    pub slack: Value,
    pub mattermost: Value,
    pub matrix: Value,
    pub signal: Value,
    pub email: Value,
    pub sms: Value,
    pub dingtalk: Value,
    pub teams: Value,
    pub ntfy: Value,
    pub simplex: Value,
    pub irc: Value,
    pub line: Value,
    pub google_chat: Value,
    pub google_meet: Value,
    pub whatsapp: Value,
    pub qqbot: Value,
    pub bluebubbles: Value,
    pub messaging_gateway: Value,
    pub spotify: Value,
    pub webhook: Value,
    pub discord: Value,
    pub moments: Value,
    pub video_summary: Value,
    pub telemetry_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            chat: ChatConfig::default(),
            reply: json!({
                "typingDelayEnabled": true,
                "typingSpeed": 0.2,
                "typingSpeedRandomMin": 0.05,
                "typingSpeedRandomMax": 0.1,
                "splitByNewline": true,
                "showTypingIndicator": true,
                "typingIndicatorRefreshSeconds": 2
            }),
            web: json!({"port": 62000, "password": "", "publicEnabled": false, "publicPort": 0, "publicSecret": ""}),
            weather: json!({"defaultLocation": "", "qweatherApiHost": "", "qweatherApiKey": "", "timeoutSeconds": 15}),
            homeassistant: json!({"enabled": false, "url": "http://homeassistant.local:8123", "token": "", "homeNotifyTarget": "", "timeoutSeconds": 15, "blockedDomains": ["shell_command", "command_line", "python_script", "pyscript", "hassio", "rest_command"]}),
            feishu: json!({"enabled": false, "baseUrl": "https://open.feishu.cn", "appId": "", "appSecret": "", "tenantAccessToken": "", "timeoutSeconds": 15}),
            yuanbao: json!({"enabled": false, "gatewayUrl": "", "token": "", "timeoutSeconds": 15, "paths": {}, "stickers": []}),
            telegram: json!({"enabled": false, "apiBaseUrl": "https://api.telegram.org", "botToken": "", "homeChannel": "", "timeoutSeconds": 15}),
            slack: json!({"enabled": false, "apiBaseUrl": "https://slack.com/api", "botToken": "", "appToken": "", "homeChannel": "", "timeoutSeconds": 15}),
            mattermost: json!({"enabled": false, "url": "", "token": "", "homeChannel": "", "replyMode": "off", "timeoutSeconds": 30}),
            matrix: json!({"enabled": false, "homeserver": "", "accessToken": "", "homeRoom": "", "timeoutSeconds": 15}),
            signal: json!({"enabled": false, "httpUrl": "http://127.0.0.1:8080", "account": "", "homeRecipient": "", "timeoutSeconds": 30}),
            email: json!({"enabled": false, "address": "", "password": "", "smtpHost": "", "smtpPort": 587, "homeAddress": "", "subject": "Hermes Agent", "timeoutSeconds": 30}),
            sms: json!({"enabled": false, "accountSid": "", "authToken": "", "fromNumber": "", "apiBaseUrl": "https://api.twilio.com", "homeNumber": "", "timeoutSeconds": 30}),
            dingtalk: json!({"enabled": false, "webhookUrl": "", "homeTarget": "", "timeoutSeconds": 30}),
            teams: json!({"enabled": false, "deliveryMode": "", "incomingWebhookUrl": "", "graphBaseUrl": "https://graph.microsoft.com/v1.0", "accessToken": "", "teamId": "", "channelId": "", "chatId": "", "homeChannel": "", "timeoutSeconds": 30}),
            ntfy: json!({"enabled": false, "server": "https://ntfy.sh", "topic": "", "publishTopic": "", "token": "", "markdown": false, "homeChannel": "", "timeoutSeconds": 15}),
            simplex: json!({"enabled": false, "wsUrl": "ws://127.0.0.1:5225", "homeChannel": "", "timeoutSeconds": 15}),
            irc: json!({"enabled": false, "server": "", "port": 6697, "nickname": "hermes-bot", "channel": "", "useTls": true, "serverPassword": "", "nickservPassword": "", "homeChannel": "", "maxMessageLength": 450, "timeoutSeconds": 15}),
            line: json!({"enabled": false, "apiBaseUrl": "https://api.line.me", "channelAccessToken": "", "channelSecret": "", "homeChannel": "", "timeoutSeconds": 15}),
            google_chat: json!({"enabled": false, "apiBaseUrl": "https://chat.googleapis.com", "accessToken": "", "serviceAccountJson": "", "credentialsFile": "", "tokenUri": "https://oauth2.googleapis.com/token", "homeChannel": "", "timeoutSeconds": 30}),
            google_meet: json!({"enabled": false, "mode": "desktop_state", "guestName": "Hermes Agent", "transcriptDir": "", "nodeRegistryPath": ""}),
            whatsapp: json!({"enabled": false, "bridgeUrl": "http://localhost:3000", "bridgePort": 3000, "homeChatId": "", "timeoutSeconds": 30}),
            qqbot: json!({"enabled": false, "appId": "", "clientSecret": "", "apiBaseUrl": "https://api.sgroup.qq.com", "tokenUrl": "https://bots.qq.com/app/getAppAccessToken", "homeTarget": "", "timeoutSeconds": 15}),
            bluebubbles: json!({"enabled": false, "serverUrl": "", "password": "", "homeChatId": "", "timeoutSeconds": 30}),
            messaging_gateway: json!({"enabled": false, "url": "", "token": "", "sendPath": "/send_message", "platforms": ["wecom", "weixin", "yuanbao"], "timeoutSeconds": 60}),
            spotify: json!({"enabled": false, "apiBaseUrl": "https://api.spotify.com/v1", "accessToken": "", "refreshToken": "", "clientId": "", "clientSecret": "", "tokenUrl": "https://accounts.spotify.com/api/token", "timeoutSeconds": 15}),
            webhook: json!({"enabled": false, "host": "127.0.0.1", "port": 8787, "path": "/webhooks/synthchat", "secret": "", "timeoutSeconds": 30}),
            discord: json!({"enabled": false, "apiBaseUrl": "https://discord.com/api/v10", "botToken": "", "gatewayUrl": "", "paths": {}, "timeoutSeconds": 15}),
            moments: json!({"autoReplyEnabled": false, "publishers": [], "repliers": []}),
            video_summary: json!({
                "enabled": false, "modelsDir": "", "transcriber": "auto", "ytDlpCommand": "yt-dlp",
                "cookie": "", "cookieFile": "", "ffmpegBinPath": "", "fasterWhisperModel": "small",
                "fasterWhisperModelDir": "", "fasterWhisperDevice": "cpu", "fasterWhisperComputeType": "int8",
                "senseVoiceModelDir": "", "senseVoiceDevice": "cpu", "timeoutSeconds": 30,
                "ytdlpInfoTimeoutSeconds": 120, "downloadTimeoutSeconds": 600, "outputDir": ""
            }),
            telemetry_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ChatConfig {
    pub skip_env_check: bool,
    pub agent_engine: String,
    pub max_context_rounds: usize,
    pub short_context_mode: String,
    pub short_context_token_budget: usize,
    pub short_context_abort_on_summary_failure: bool,
    pub short_context_summary_provider_id: String,
    pub short_context_summary_model: String,
    pub busy_input_mode: String,
    pub auto_title_enabled: bool,
    pub queue_wait_seconds: u64,
    pub delegation_max_concurrent_children: u32,
    pub delegation_strategy: String,
    pub delegation_orchestrator_enabled: bool,
    pub delegation_subagent_auto_approve: bool,
    pub delegation_inherit_mcp_toolsets: bool,
    pub delegation_subagent_provider_id: String,
    pub delegation_subagent_model: String,
    pub auxiliary_task_assignments: Value,
    pub agent_run_timeout_seconds: u64,
    pub agent_post_tool_quiet_timeout_seconds: u64,
    pub ui_message_limit: usize,
    pub artifact_scan_limit: usize,
    pub ui_message_preview_chars: usize,
    pub ui_stream_chars_per_second: usize,
    pub thinking_min_visible_ms: usize,
    pub pet_cloud_duration_seconds: usize,
    pub bottom_follow_threshold_px: usize,
    pub active_poll_interval_ms: usize,
    pub idle_poll_interval_ms: usize,
    pub intent_analyzer_mode: String,
    pub tool_router_mode: String,
    pub tool_use_enforcement: String,
    pub tool_parallel_enabled: bool,
    pub tool_parallel_limit: usize,
    pub send_message_tool_enabled: bool,
    pub tool_approval_mode: String,
    pub cron_approval_mode: String,
    pub trusted_tool_patterns: Vec<String>,
    pub trusted_command_patterns: Vec<String>,
    pub hooks: Value,
    pub hooks_auto_accept: bool,
    pub llm_credential_pool_strategy: String,
    pub tool_env_passthrough: Vec<String>,
    pub tool_credential_files: Vec<String>,
    pub tool_mutation_checkpoint_enabled: bool,
    pub llm_retry_count: usize,
    pub llm_retry_backoff_ms: usize,
    pub responses_reasoning_replay_enabled: bool,
    pub fast_mode_enabled: bool,
    pub runtime_footer_enabled: bool,
    pub statusbar_enabled: bool,
    pub tool_progress_display: String,
    pub display_skin: String,
    pub busy_indicator_style: String,
    pub codex_runtime: String,
    pub tool_call_retry_count: usize,
    pub tool_call_retry_backoff_ms: usize,
    pub tool_result_persist_threshold_chars: usize,
    pub tool_result_preview_chars: usize,
    pub tool_observation_turn_budget_chars: usize,
    pub tool_observation_tail_budget_chars: usize,
    pub tool_output_max_bytes: usize,
    pub tool_output_max_lines: usize,
    pub tool_output_max_line_length: usize,
    pub tool_guardrail_warnings_enabled: bool,
    pub tool_guardrail_hard_stop_enabled: bool,
    pub tool_guardrail_exact_failure_warn_after: u32,
    pub tool_guardrail_same_tool_failure_warn_after: u32,
    pub tool_guardrail_no_progress_warn_after: u32,
    pub tool_guardrail_exact_failure_limit: u32,
    pub tool_guardrail_same_tool_failure_limit: u32,
    pub tool_guardrail_no_progress_limit: u32,
    pub background_memory_review_enabled: bool,
    pub background_memory_review_min_messages: usize,
    pub background_skill_review_enabled: bool,
    pub background_skill_review_auto_create_enabled: bool,
    pub background_skill_curator_enabled: bool,
    pub background_skill_curator_interval_hours: usize,
    pub skill_hot_reload_enabled: bool,
    pub skill_hot_reload_interval_seconds: usize,
    pub history_cleanup_enabled: bool,
    pub history_retention_days: usize,
    pub max_stored_messages_per_conversation: usize,
    pub max_stored_agent_runs: usize,
    pub max_stored_tool_traces: usize,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            skip_env_check: true,
            agent_engine: "rust_synthgraph".into(),
            max_context_rounds: 10,
            short_context_mode: "messages".into(),
            short_context_token_budget: 8000,
            short_context_abort_on_summary_failure: false,
            short_context_summary_provider_id: String::new(),
            short_context_summary_model: String::new(),
            busy_input_mode: "queue".into(),
            auto_title_enabled: true,
            queue_wait_seconds: 7,
            delegation_max_concurrent_children: 3,
            delegation_strategy: "auto".into(),
            delegation_orchestrator_enabled: true,
            delegation_subagent_auto_approve: false,
            delegation_inherit_mcp_toolsets: true,
            delegation_subagent_provider_id: String::new(),
            delegation_subagent_model: String::new(),
            auxiliary_task_assignments: json!({}),
            agent_run_timeout_seconds: 600,
            agent_post_tool_quiet_timeout_seconds: 90,
            ui_message_limit: 180,
            artifact_scan_limit: 80,
            ui_message_preview_chars: 12000,
            ui_stream_chars_per_second: 36,
            thinking_min_visible_ms: 1800,
            pet_cloud_duration_seconds: 10,
            bottom_follow_threshold_px: 180,
            active_poll_interval_ms: 1500,
            idle_poll_interval_ms: 3000,
            intent_analyzer_mode: "embedding".into(),
            tool_router_mode: "llm_unified".into(),
            tool_use_enforcement: "auto".into(),
            tool_parallel_enabled: true,
            tool_parallel_limit: 8,
            send_message_tool_enabled: false,
            tool_approval_mode: "risky".into(),
            cron_approval_mode: "deny".into(),
            trusted_tool_patterns: vec![],
            trusted_command_patterns: vec![],
            hooks: json!({}),
            hooks_auto_accept: false,
            llm_credential_pool_strategy: "fill_first".into(),
            tool_env_passthrough: vec![],
            tool_credential_files: vec![],
            tool_mutation_checkpoint_enabled: true,
            llm_retry_count: 2,
            llm_retry_backoff_ms: 800,
            responses_reasoning_replay_enabled: true,
            fast_mode_enabled: false,
            runtime_footer_enabled: false,
            statusbar_enabled: true,
            tool_progress_display: "new".into(),
            display_skin: "default".into(),
            busy_indicator_style: "unicode".into(),
            codex_runtime: "auto".into(),
            tool_call_retry_count: 1,
            tool_call_retry_backoff_ms: 300,
            tool_result_persist_threshold_chars: 24_000,
            tool_result_preview_chars: 6_000,
            tool_observation_turn_budget_chars: 200_000,
            tool_observation_tail_budget_chars: 80_000,
            tool_output_max_bytes: 50_000,
            tool_output_max_lines: 2_000,
            tool_output_max_line_length: 2_000,
            tool_guardrail_warnings_enabled: true,
            tool_guardrail_hard_stop_enabled: false,
            tool_guardrail_exact_failure_warn_after: 2,
            tool_guardrail_same_tool_failure_warn_after: 3,
            tool_guardrail_no_progress_warn_after: 2,
            tool_guardrail_exact_failure_limit: 5,
            tool_guardrail_same_tool_failure_limit: 8,
            tool_guardrail_no_progress_limit: 5,
            background_memory_review_enabled: true,
            background_memory_review_min_messages: 4,
            background_skill_review_enabled: true,
            background_skill_review_auto_create_enabled: false,
            background_skill_curator_enabled: true,
            background_skill_curator_interval_hours: 168,
            skill_hot_reload_enabled: true,
            skill_hot_reload_interval_seconds: 3,
            history_cleanup_enabled: true,
            history_retention_days: 14,
            max_stored_messages_per_conversation: 300,
            max_stored_agent_runs: 50,
            max_stored_tool_traces: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfig {
    pub name: String,
    pub avatar_path: Option<String>,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            name: "用户".into(),
            avatar_path: Some(String::new()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiGroupConfig {
    pub id: String,
    pub name: String,
    pub emotions: Vec<String>,
    pub images: Vec<String>,
    #[serde(default)]
    pub emotion_images: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct Persona {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub avatar_path: Option<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub character_prompt: String,
    #[serde(default)]
    pub output_examples: String,
    #[serde(default = "default_system_instructions")]
    pub system_instructions: String,
    #[serde(default)]
    pub llm_provider: String,
    #[serde(default)]
    pub llm_model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_tool_policy")]
    pub tool_policy: Value,
    #[serde(default)]
    pub emoji_enabled: bool,
    #[serde(default)]
    pub emoji_group: String,
    #[serde(default = "default_emoji_send_probability")]
    pub emoji_send_probability: u8,
    #[serde(default = "default_memory_config")]
    pub memory: Value,
    #[serde(default = "default_proactive_config")]
    pub proactive: Value,
    #[serde(default = "default_voice_reply_config")]
    pub voice_reply: Value,
    #[serde(default = "default_image_generation_config")]
    pub image_generation: Value,
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            id: "default".into(),
            name: "小可".into(),
            agent_id: "default".into(),
            avatar_path: Some(String::new()),
            system_prompt: "你是一个友好、稳定的聊天助手。".into(),
            character_prompt: String::new(),
            output_examples: String::new(),
            system_instructions: default_system_instructions(),
            llm_provider: String::new(),
            llm_model: String::new(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            tool_policy: default_tool_policy(),
            emoji_enabled: true,
            emoji_group: "default".into(),
            emoji_send_probability: default_emoji_send_probability(),
            memory: default_memory_config(),
            proactive: default_proactive_config(),
            voice_reply: default_voice_reply_config(),
            image_generation: default_image_generation_config(),
        }
    }
}

fn default_system_instructions() -> String {
    "请始终保持角色一致性，结合角色详情、世界书与长期记忆作答。".into()
}

fn default_temperature() -> f32 {
    0.8
}

fn default_max_tokens() -> u32 {
    2048
}

fn default_tool_policy() -> Value {
    json!({"enabled": true, "timeoutSeconds": 30, "maxIterations": 90, "maxFailureReplans": 2, "retryCount": 1, "retryBackoffMs": 300})
}

fn default_emoji_send_probability() -> u8 {
    25
}

fn default_memory_config() -> Value {
    json!({"enabled": true, "triggerRounds": 10, "maxMemories": 50, "includeInPrompt": true})
}

fn default_proactive_config() -> Value {
    json!({"enabled": false, "minIdleHours": 1, "maxIdleHours": 3, "maxConsecutive": 3, "prompt": "用户已经一段时间没有回复了。请根据角色设定与近期对话，主动发起一条贴合角色的简短消息。", "quietHours": {"enabled": true, "start": "22:00", "end": "08:00"}})
}

fn default_voice_reply_config() -> Value {
    json!({"enabled": false, "engine": "chattts", "language": "zh-CN", "voice": "zh-CN-XiaoxiaoNeural", "volume": "+0%", "pitch": "+0Hz", "pythonPath": "", "modelDir": "", "sampleRate": 16000, "speed": 5, "oral": 2, "laugh": 0, "breakLevel": 4, "speakerSeed": 20240, "speakerEmbedding": "", "temperature": 0.3, "topP": 0.7, "topK": 20, "refineTextEnabled": true, "refinePrompt": "[oral_2][laugh_0][break_4]", "refineTemperature": 0.7})
}

fn default_image_generation_config() -> Value {
    json!({"enabled": false, "provider": "", "model": "", "stylePrefix": "", "artStyle": "anime style, masterpiece, best quality", "negativePrompt": "low quality, blurry, watermark, text, signature, lowres, bad anatomy, extra fingers, jpeg artifacts", "negativeEnabled": true, "refMode": "avatar"})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub last_message: String,
    pub persona_id: Option<String>,
    pub wechat_account_id: Option<String>,
    pub agent_id: String,
    pub created_at: String,
    #[serde(default)]
    pub metadata: Value,
}

impl Conversation {
    pub fn new(title: String, persona_id: String, agent_id: String) -> Self {
        let now = now_iso();
        Self {
            id: new_id("conv"),
            title,
            updated_at: now.clone(),
            last_message: String::new(),
            persona_id: Some(persona_id),
            wechat_account_id: None,
            agent_id,
            created_at: now,
            metadata: json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub source: String,
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactiveStatus {
    pub persona_id: String,
    pub persona_name: String,
    pub enabled: bool,
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub conversation_busy: bool,
    pub last_user_at: i64,
    pub seconds_since_last_user: i64,
    pub last_reply_at: i64,
    pub seconds_since_last_reply: i64,
    pub wait_seconds: u64,
    pub ready_in_seconds: i64,
    pub consecutive_count: u32,
    pub max_consecutive: u32,
    pub in_quiet_hours: bool,
    #[serde(default)]
    pub pet_vision_suspended: bool,
    pub can_fire: bool,
    pub blocked_reason: String,
}

impl ChatMessage {
    pub fn new(conversation_id: String, role: &str, content: String, source: &str) -> Self {
        Self {
            id: new_id("msg"),
            conversation_id,
            role: role.into(),
            content,
            created_at: now_iso(),
            source: source.into(),
            account_id: None,
            provider_data: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatRequest {
    pub conversation_id: Option<String>,
    pub persona_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_item_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub preset: Option<String>,
    pub base_url: String,
    pub append_chat_path: bool,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
    pub enabled: bool,
    pub timeout_seconds: u64,
    #[serde(
        default,
        alias = "request_timeout_seconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_timeout_seconds: Option<f64>,
    #[serde(
        default,
        alias = "stale_timeout_seconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub stale_timeout_seconds: Option<f64>,
    #[serde(default)]
    pub models: Value,
    #[serde(default = "default_prompt_cache_mode")]
    pub prompt_cache_mode: String,
    #[serde(default = "default_prompt_cache_ttl")]
    pub prompt_cache_ttl: String,
    #[serde(default = "default_prompt_cache_layout")]
    pub prompt_cache_layout: String,
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self {
            id: "local-echo".into(),
            name: "本地回显".into(),
            provider_type: "echo".into(),
            preset: Some("echo".into()),
            base_url: String::new(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "echo".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: default_prompt_cache_mode(),
            prompt_cache_ttl: default_prompt_cache_ttl(),
            prompt_cache_layout: default_prompt_cache_layout(),
        }
    }
}

fn default_prompt_cache_mode() -> String {
    "auto".into()
}

fn default_prompt_cache_ttl() -> String {
    "5m".into()
}

fn default_prompt_cache_layout() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
    pub enabled: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub enabled: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
    pub enabled: bool,
    pub timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub use_system_proxy: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
    pub enabled: bool,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub submit_path: String,
    #[serde(default)]
    pub status_path: String,
    #[serde(default)]
    pub id_path: String,
    #[serde(default)]
    pub status_field: String,
    #[serde(default)]
    pub result_path: String,
    #[serde(default)]
    pub completed_statuses: Vec<String>,
    #[serde(default)]
    pub failed_statuses: Vec<String>,
    #[serde(default)]
    pub poll_interval_seconds: u64,
    #[serde(default)]
    pub max_poll_seconds: u64,
    #[serde(default)]
    pub download_result: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProvider {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub record_sessions: bool,
    pub enabled: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub workspace_dir: String,
    pub llm_provider: String,
    pub llm_model: String,
    pub enabled: bool,
    pub is_default: bool,
    pub mcp_enabled: bool,
    pub skills_enabled: bool,
    pub allow_shell: bool,
    pub max_subagents: u32,
    #[serde(default = "default_max_subagent_depth")]
    pub max_subagent_depth: u32,
    pub max_tool_iterations: u32,
    pub skills_dir: String,
    pub enabled_skills: Vec<String>,
    pub enabled_mcp_servers: Vec<String>,
    #[serde(default)]
    pub enabled_toolsets: Vec<String>,
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn default_max_subagent_depth() -> u32 {
    1
}

impl Default for AgentDefinition {
    fn default() -> Self {
        let now = now_iso();
        Self {
            id: "default".into(),
            name: "默认智能体".into(),
            description: "SynthChat Rust 对话智能体".into(),
            workspace_dir: String::new(),
            llm_provider: String::new(),
            llm_model: String::new(),
            enabled: true,
            is_default: true,
            mcp_enabled: true,
            skills_enabled: true,
            allow_shell: true,
            max_subagents: 4,
            max_subagent_depth: default_max_subagent_depth(),
            max_tool_iterations: 90,
            skills_dir: String::new(),
            enabled_skills: vec![],
            enabled_mcp_servers: vec![],
            enabled_toolsets: vec![],
            disabled_toolsets: vec![],
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancedSkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub path: String,
    pub version: String,
    pub author: String,
    pub icon: String,
    pub is_core: bool,
    pub is_bundled: bool,
    pub source: String,
    pub agent_id: String,
    pub config: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub required_environment_variables: Vec<String>,
    #[serde(default)]
    pub required_credential_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillBundle {
    pub id: String,
    pub name: String,
    pub description: String,
    pub skill_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub download_url: String,
    pub icon: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAuditFinding {
    pub severity: String,
    pub category: String,
    pub message: String,
    pub file: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAuditReport {
    pub skill_id: String,
    pub name: String,
    pub path: String,
    pub status: String,
    pub checked_files: usize,
    pub findings: Vec<SkillAuditFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallRecord {
    pub skill_id: String,
    pub name: String,
    pub source: String,
    pub identifier: String,
    pub install_path: String,
    pub audit_status: String,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillTap {
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillTapStatus {
    pub repo: String,
    pub path: String,
    pub status: String,
    pub entry_count: usize,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdateCheck {
    pub skill_id: String,
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCuratorOverlap {
    pub umbrella: String,
    pub skill_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCuratorArchiveCandidate {
    pub skill_id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCuratorReport {
    pub generated_at: String,
    pub report_path: String,
    pub total_skills: usize,
    pub external_skills: usize,
    pub bundled_skills: usize,
    pub audit_attention: usize,
    pub overlap_clusters: Vec<SkillCuratorOverlap>,
    pub archive_candidates: Vec<SkillCuratorArchiveCandidate>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCuratorArchiveRecord {
    pub archive_id: String,
    pub skill_id: String,
    pub name: String,
    pub original_path: String,
    pub archive_path: String,
    pub reason: String,
    pub archived_at: String,
    pub restored_at: Option<String>,
    pub install_record: SkillInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCuratorState {
    pub paused: bool,
    pub pinned_skill_ids: Vec<String>,
    pub archived: Vec<SkillCuratorArchiveRecord>,
    pub last_run_at: Option<String>,
    pub last_report_path: Option<String>,
    pub run_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SkillPromptBlock {
    pub id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    #[serde(default)]
    pub provided_tools: Vec<String>,
    #[serde(default)]
    pub provided_capabilities: Vec<String>,
    #[serde(default)]
    pub provided_hooks: Vec<String>,
    #[serde(default)]
    pub requires_env: Vec<String>,
    #[serde(default)]
    pub missing_env: Vec<String>,
    #[serde(default)]
    pub env_configured: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub homepage_url: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub manifest_path: String,
    #[serde(default)]
    pub entry_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAuxiliaryTaskSummary {
    pub plugin_id: String,
    pub plugin_name: String,
    pub key: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub defaults: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuxiliaryTaskSummary {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub source: String,
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default)]
    pub defaults: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuxiliaryTaskAssignment {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub source: String,
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub plugin_name: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub timeout: u64,
    #[serde(default)]
    pub extra_body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGoalState {
    pub goal: String,
    pub status: String,
    pub turns_used: u32,
    pub max_turns: u32,
    pub created_at: String,
    pub last_turn_at: Option<String>,
    pub last_verdict: Option<String>,
    pub last_reason: Option<String>,
    pub paused_reason: Option<String>,
    pub consecutive_parse_failures: u32,
    #[serde(default)]
    pub subgoals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRecord {
    pub run_id: String,
    pub conversation_id: String,
    pub persona_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub subagent_index: Option<u32>,
    #[serde(default)]
    pub subagent_depth: Option<u32>,
    #[serde(default)]
    pub subagent_can_delegate: Option<bool>,
    #[serde(default)]
    pub subagent_role: Option<String>,
    #[serde(default)]
    pub subagent_task: Option<String>,
    #[serde(default)]
    pub subagent_toolsets: Vec<String>,
    #[serde(default)]
    pub subagent_max_iterations: Option<u32>,
    #[serde(default)]
    pub user_request: String,
    #[serde(default)]
    pub queue_item_id: Option<String>,
    pub state: String,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_activity_at: Option<String>,
    #[serde(default)]
    pub last_activity_desc: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub tool_events: Vec<Value>,
    #[serde(default)]
    pub phase_events: Vec<AgentRunPhaseRecord>,
    #[serde(default)]
    pub checkpoints: Vec<AgentCheckpointRecord>,
    #[serde(default)]
    pub pending_steers: Vec<String>,
}

impl AgentRunRecord {
    pub fn new(conversation_id: String, persona_id: String, agent_id: String) -> Self {
        let now = now_iso();
        Self {
            run_id: new_id("run"),
            conversation_id,
            persona_id,
            agent_id,
            parent_run_id: None,
            subagent_index: None,
            subagent_depth: None,
            subagent_can_delegate: None,
            subagent_role: None,
            subagent_task: None,
            subagent_toolsets: vec![],
            subagent_max_iterations: None,
            user_request: String::new(),
            queue_item_id: None,
            state: "started".into(),
            started_at: now.clone(),
            updated_at: now.clone(),
            last_activity_at: Some(now),
            last_activity_desc: Some("starting new turn".into()),
            completed_at: None,
            error: None,
            tool_events: vec![],
            phase_events: vec![],
            checkpoints: vec![],
            pending_steers: vec![],
        }
    }

    pub fn touch_activity(&mut self, description: impl Into<String>) {
        let now = now_iso();
        self.updated_at = now.clone();
        self.last_activity_at = Some(now);
        self.last_activity_desc = Some(description.into());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQueuedRequest {
    pub id: String,
    pub conversation_id: String,
    pub persona_id: String,
    pub user_message_id: String,
    pub content: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data: Option<Value>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
}

impl AgentQueuedRequest {
    pub fn new(conversation_id: String, persona_id: String, user_message: &ChatMessage) -> Self {
        let now = now_iso();
        Self {
            id: new_id("queue"),
            conversation_id,
            persona_id,
            user_message_id: user_message.id.clone(),
            content: user_message.content.clone(),
            source: user_message.source.clone(),
            provider_data: user_message.provider_data.clone(),
            status: "pending".into(),
            created_at: now.clone(),
            updated_at: now,
            started_at: None,
            completed_at: None,
            error: None,
        }
    }

    pub fn request_provider_data(&self) -> Option<Value> {
        self.provider_data.clone().or_else(|| {
            let source = self.source.trim();
            if source.is_empty() {
                None
            } else {
                Some(json!({ "source": source }))
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ScheduledAgentJob {
    pub id: String,
    pub name: String,
    pub conversation_id: Option<String>,
    pub persona_id: String,
    pub profile: Option<String>,
    pub agent_id: Option<String>,
    pub prompt: String,
    pub skill: Option<String>,
    pub skills: Vec<String>,
    pub context_from: Vec<String>,
    pub script: Option<String>,
    pub no_agent: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub workdir: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub script_timeout_seconds: Option<u64>,
    pub deliver: Option<String>,
    pub origin: Option<Value>,
    pub schedule_kind: String,
    pub schedule_display: String,
    pub interval_minutes: Option<u64>,
    pub cron_expr: Option<String>,
    pub run_at: Option<String>,
    pub repeat: Option<u64>,
    pub enabled_toolsets: Vec<String>,
    pub disabled_toolsets: Vec<String>,
    pub enabled: bool,
    pub status: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_run_status: Option<String>,
    pub last_output: Option<String>,
    pub last_output_path: Option<String>,
    pub last_error: Option<String>,
    pub last_delivery_error: Option<String>,
    pub run_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJobOutputRecord {
    pub file_name: String,
    pub path: String,
    pub modified_at: String,
    pub size_bytes: u64,
    pub status: String,
}

impl Default for ScheduledAgentJob {
    fn default() -> Self {
        let now = now_iso();
        Self {
            id: String::new(),
            name: String::new(),
            conversation_id: None,
            persona_id: "default".into(),
            profile: None,
            agent_id: None,
            prompt: String::new(),
            skill: None,
            skills: vec![],
            context_from: vec![],
            script: None,
            no_agent: false,
            provider: None,
            model: None,
            base_url: None,
            workdir: None,
            timeout_seconds: None,
            script_timeout_seconds: None,
            deliver: None,
            origin: None,
            schedule_kind: "once".into(),
            schedule_display: String::new(),
            interval_minutes: None,
            cron_expr: None,
            run_at: None,
            repeat: None,
            enabled_toolsets: vec![],
            disabled_toolsets: vec![],
            enabled: true,
            status: "scheduled".into(),
            next_run_at: None,
            last_run_at: None,
            last_completed_at: None,
            last_run_status: None,
            last_output: None,
            last_output_path: None,
            last_error: None,
            last_delivery_error: None,
            run_count: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTodoItem {
    pub id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub content: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentTodoItem {
    pub fn new(run_id: String, conversation_id: String, content: String, status: String) -> Self {
        let now = now_iso();
        Self {
            id: new_id("todo"),
            run_id,
            conversation_id,
            content,
            status,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunPhaseRecord {
    pub phase: String,
    pub detail: Value,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCheckpointRecord {
    pub checkpoint_id: String,
    pub run_id: String,
    pub iteration: u32,
    pub created_at: String,
    pub state: String,
    #[serde(default)]
    pub completed_call_ids: Vec<String>,
    #[serde(default)]
    pub event_refs: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub persona_id: String,
    #[serde(default = "default_memory_target")]
    pub target: String,
    pub summary: String,
    pub importance: u8,
    pub created_at: String,
    pub updated_at: String,
}

pub fn default_memory_target() -> String {
    "memory".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatus {
    pub persona_id: String,
    pub persona_name: String,
    pub enabled: bool,
    pub include_in_prompt: bool,
    pub trigger_rounds: u64,
    pub max_memories: u64,
    pub total: usize,
    pub prompt_safe: usize,
    pub blocked_by_security_scan: usize,
    pub prompt_injected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortContextState {
    pub conversation_id: String,
    pub boundary_id: Option<String>,
    pub summary: String,
    pub summary_tokens: usize,
    pub summary_messages: usize,
    #[serde(default = "default_compression_savings_pct")]
    pub last_compression_savings_pct: f64,
    #[serde(default)]
    pub ineffective_compression_count: usize,
    #[serde(default)]
    pub last_real_prompt_tokens: usize,
    #[serde(default)]
    pub last_compression_rough_tokens: usize,
    #[serde(default)]
    pub last_rough_tokens_when_real_prompt_fit: usize,
    #[serde(default)]
    pub awaiting_real_usage_after_compression: bool,
    #[serde(default)]
    pub summary_failure_cooldown_until_ms: u64,
    #[serde(default)]
    pub last_summary_error: Option<String>,
    #[serde(default)]
    pub last_summary_fallback_used: bool,
    #[serde(default)]
    pub last_summary_dropped_count: usize,
    #[serde(default)]
    pub last_compress_aborted: bool,
    #[serde(default)]
    pub last_aux_summary_error: Option<String>,
    #[serde(default)]
    pub last_aux_summary_model: Option<String>,
}

fn default_compression_savings_pct() -> f64 {
    100.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub protocol: String,
    pub enabled: bool,
    pub timeout_seconds: u64,
    #[serde(default, alias = "supports_parallel_tool_calls")]
    pub supports_parallel_tool_calls: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityAdapter {
    pub name: String,
    pub description: String,
    pub mcp_server: String,
    pub mcp_tool: String,
    pub parameters: Value,
    pub param_mapping: std::collections::HashMap<String, String>,
    pub inject_fields: std::collections::HashMap<String, String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListToolsResult {
    pub ok: bool,
    pub timed_out: bool,
    pub elapsed_ms: u128,
    pub tools: Vec<McpToolInfo>,
    pub raw: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallResult {
    pub ok: bool,
    pub timed_out: bool,
    pub elapsed_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolEvent {
    pub status: Option<String>,
    pub reference_id: Option<String>,
    pub call_id: Option<String>,
    pub run_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub event_type: String,
    pub server_id: String,
    pub tool_name: String,
    pub ok: bool,
    pub timed_out: bool,
    pub elapsed_ms: u128,
    #[serde(default = "default_tool_event_kind")]
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub path: Option<String>,
    pub exists: Option<bool>,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub error: Option<String>,
    pub raw: Option<Value>,
}

fn default_tool_event_kind() -> String {
    "other".into()
}

pub fn tool_event_kind(server_id: &str, tool_name: &str, description: Option<&str>) -> String {
    let name = tool_name.to_lowercase();
    match name.as_str() {
        "tool_describe"
        | "read_file"
        | "browser_snapshot"
        | "browser_vision"
        | "browser_get_images"
        | "browser_plugins"
        | "transcribe_audio"
        | "voice_status"
        | "vision_analyze"
        | "video_analyze"
        | "skill_view"
        | "skills_list"
        | "recall_memory"
        | "session_search"
        | "manage_memory"
        | "memory"
        | "memory_provider"
        | "dashboard_auth"
        | "dashboard_plugins"
        | "context_engine"
        | "plugin_runtime"
        | "teams_pipeline"
        | "provider_plugins"
        | "fact_store"
        | "fact_feedback"
        | "supermemory_profile"
        | "honcho_profile"
        | "honcho_context"
        | "mem0_profile"
        | "viking_read"
        | "viking_browse"
        | "byterover_status"
        | "brv_status"
        | "retaindb_profile"
        | "retaindb_list_files"
        | "retaindb_read_file"
        | "retaindb_agent_model"
        | "kanban_show"
        | "kanban_list"
        | "ha_list_entities"
        | "ha_get_state"
        | "ha_list_services"
        | "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "yb_query_group_info"
        | "yb_query_group_members"
        | "spotify_albums"
        | "spotify_status"
        | "discord"
        | "browser_supervisor_state" => return "read".into(),
        "write_file"
        | "patch"
        | "skill_manage"
        | "remember_fact"
        | "supermemory_store"
        | "supermemory_forget"
        | "honcho_conclude"
        | "mem0_conclude"
        | "viking_remember"
        | "viking_add_resource"
        | "brv_curate"
        | "hindsight_reflect"
        | "hindsight_remember"
        | "retaindb_store"
        | "retaindb_remember"
        | "retaindb_forget"
        | "retaindb_upload_file"
        | "retaindb_ingest_file"
        | "retaindb_delete_file"
        | "retaindb_ingest_session"
        | "retaindb_seed_agent"
        | "cronjob"
        | "send_message"
        | "teams_typing"
        | "mattermost_typing"
        | "google_chat_typing"
        | "google_chat_update_message"
        | "update_todo"
        | "ha_call_service"
        | "kanban_create"
        | "kanban_specify"
        | "kanban_update"
        | "kanban_delete"
        | "kanban_complete"
        | "kanban_block"
        | "kanban_unblock"
        | "kanban_heartbeat"
        | "kanban_comment"
        | "kanban_link"
        | "kanban_unlink"
        | "kanban_bulk_update"
        | "mcp_oauth_clear"
        | "mcp_oauth_refresh"
        | "feishu_drive_update_comment_reaction"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment"
        | "yb_send_dm"
        | "yb_send_sticker"
        | "discord_admin" => return "edit".into(),
        "tool_search" | "search_files" | "yb_search_sticker" | "spotify_search"
        | "supermemory_search" | "honcho_search" | "mem0_search" | "viking_search"
        | "brv_query" | "hindsight_search" | "retaindb_search" => {
            return "search".into();
        }
        "web_extract" | "web_search" | "x_search" | "browser_navigate" | "browser_cdp"
        | "weather" | "osv_check" | "security_scan" | "mcp_status" | "trace_flush"
        | "honcho_reasoning" | "retaindb_context" => {
            return "fetch".into();
        }
        "terminal"
        | "tool_call"
        | "shell"
        | "process"
        | "execute_code"
        | "workspace_diagnostics"
        | "computer_use"
        | "mcp_probe"
        | "mcp_reset_session"
        | "disk_cleanup"
        | "spotify_playback"
        | "spotify_devices"
        | "spotify_queue"
        | "spotify_playlists"
        | "spotify_library"
        | "browser_click"
        | "browser_type"
        | "browser_press"
        | "browser_scroll"
        | "browser_back"
        | "browser_record"
        | "delegate_task"
        | "mixture_of_agents"
        | "image_generate"
        | "video_generate"
        | "text_to_speech"
        | "voice_playback"
        | "voice_recording"
        | "clarify" => return "execute".into(),
        "_thinking" => return "think".into(),
        _ => {}
    }
    let text = format!(
        "{} {} {}",
        server_id.to_lowercase(),
        name,
        description.unwrap_or("").to_lowercase()
    );
    if text.contains("search") || text.contains("find") || text.contains("query") {
        "search".into()
    } else if text.contains("read")
        || text.contains("list")
        || text.contains("get")
        || text.contains("snapshot")
        || text.contains("inspect")
    {
        "read".into()
    } else if text.contains("write")
        || text.contains("edit")
        || text.contains("patch")
        || text.contains("delete")
        || text.contains("remove")
        || text.contains("save")
    {
        "edit".into()
    } else if text.contains("http")
        || text.contains("web")
        || text.contains("url")
        || text.contains("fetch")
    {
        "fetch".into()
    } else if text.contains("exec") || text.contains("command") || text.contains("run") {
        "execute".into()
    } else {
        "other".into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTraceEntry {
    pub id: String,
    pub created_at: String,
    pub server_id: String,
    pub tool_name: String,
    pub ok: bool,
    pub timed_out: bool,
    pub elapsed_ms: u128,
    pub payload: Value,
    pub event: ToolEvent,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStateRecord {
    pub path: String,
    pub sha256: String,
    pub modified_unix_ms: u128,
    pub bytes: usize,
    pub partial: bool,
    pub readers: Vec<FileStateReaderRecord>,
    pub last_reader: Option<String>,
    pub last_reader_run_id: Option<String>,
    pub last_read_at: Option<String>,
    pub last_write_at: Option<String>,
    pub last_writer: Option<String>,
    pub last_writer_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStateReaderRecord {
    pub actor: String,
    pub run_id: Option<String>,
    pub read_at: String,
    pub sha256: String,
    pub modified_unix_ms: u128,
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub source: String,
    pub server_id: String,
    pub tool_name: String,
    pub input_schema: Value,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalRequest {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
    pub conversation_id: Option<String>,
    pub persona_id: Option<String>,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    pub server_id: String,
    pub tool_name: String,
    pub payload: Value,
    pub reason: String,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannerTraceRecord {
    pub id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub persona_id: String,
    pub agent_id: String,
    pub iteration: u32,
    pub created_at: String,
    pub input: String,
    pub output: String,
    pub parsed_step: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTraceRecord {
    pub id: String,
    pub created_at: String,
    pub conversation_id: String,
    pub persona_id: String,
    pub agent_id: String,
    pub semantic_intent: String,
    pub user_request: String,
    pub prompt: String,
    pub output: String,
    pub decision: Option<Value>,
    pub status: String,
    pub error: Option<String>,
}
