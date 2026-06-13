use std::{collections::HashSet, env, fs, path::PathBuf, time::Duration as StdDuration};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    models::{now_iso, ChatMessage},
    store::AppStore,
};

const DEFAULT_STORE_FILENAME: &str = "teams_pipeline_store.json";

pub(super) fn teams_pipeline_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    let store_path = teams_pipeline_store_path(store, payload);
    let state = read_store_state(&store_path);
    let value = match action.as_str() {
        "" | "status" | "validate" | "diagnostics" => validate_snapshot(store, &store_path, &state),
        "list" | "ls" => list_jobs(payload, &store_path, &state),
        "show" => show_job(payload, &store_path, &state),
        "subscriptions" | "subs" => list_subscriptions(&store_path, &state),
        "upsert-subscription" | "store-upsert-subscription" => {
            upsert_subscription(payload, &store_path, state)?
        }
        "delete-local-subscription" | "store-delete-subscription" => {
            delete_local_subscription(payload, &store_path, state)?
        }
        "upsert-job" | "store-upsert-job" => upsert_job(payload, &store_path, state)?,
        "upsert-sink-record" | "store-upsert-sink-record" => {
            upsert_sink_record(payload, &store_path, state)?
        }
        "get-sink-record" | "store-get-sink-record" => {
            get_sink_record(payload, &store_path, &state)
        }
        "receipt-key" | "build-notification-receipt-key" => notification_receipt_key(payload),
        "has-notification-receipt" | "store-has-notification-receipt" => {
            has_notification_receipt(payload, &store_path, &state)
        }
        "record-notification-receipt" | "store-record-notification-receipt" => {
            record_notification_receipt(payload, &store_path, state)?
        }
        "record-event-timestamp" | "store-record-event-timestamp" => {
            record_event_timestamp(payload, &store_path, state)?
        }
        "get-event-timestamp" | "store-get-event-timestamp" => {
            get_event_timestamp(payload, &store_path, &state)
        }
        "webhook-validation" | "validation-token" => webhook_validation(payload),
        "webhook-notification" | "process-webhook-notification" => {
            process_webhook_notification(payload, &store_path, state)?
        }
        "schedule-received" | "schedule" | "scheduler" => {
            schedule_received_jobs(store, payload, &store_path, state)?
        }
        "gateway-runtime" | "scheduler-runtime" | "runtime-plan" | "gateway-plan"
        | "gateway-stop" | "scheduler-stop" | "runtime-stop" | "gateway-restart"
        | "scheduler-restart" | "runtime-restart" => {
            gateway_scheduler_runtime_plan(store, payload, &state)
        }
        "token" | "token_health" | "token-health" => token_health_snapshot(payload),
        "fetch" | "test" => plan_graph_fetch(payload, &store_path, &state)?,
        "run" | "replay" => plan_pipeline_run(store, payload, &store_path, &state)?,
        "summarize" | "generate-summary" | "summary-prompt" => {
            plan_pipeline_summary_generation(store, payload, &store_path, &state)?
        }
        "write-sinks" | "plan-sinks" | "replay-sinks" => {
            plan_pipeline_sink_write(store, payload, &store_path, &state)?
        }
        "subscribe" => plan_graph_subscribe(payload, &store_path, &state)?,
        "renew-subscription" => plan_graph_renew_subscription(payload, &store_path, &state)?,
        "delete-subscription" => plan_graph_delete_subscription(payload, &store_path, &state)?,
        "maintain-subscriptions" => {
            plan_graph_subscription_maintenance(payload, &store_path, &state)?
        }
        other => json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "plugin": "teams_pipeline",
            "status": "unsupported_action",
            "action": other,
            "supportedActions": [
                "status", "validate", "list", "show", "subscriptions", "token-health",
                "upsert-subscription", "delete-local-subscription", "upsert-job",
                "upsert-sink-record", "get-sink-record", "receipt-key",
                "has-notification-receipt", "record-notification-receipt",
                "record-event-timestamp", "get-event-timestamp",
                "webhook-validation", "webhook-notification", "schedule-received",
                "gateway-runtime", "scheduler-runtime", "runtime-plan", "gateway-plan",
                "gateway-stop", "scheduler-stop", "runtime-stop",
                "gateway-restart", "scheduler-restart", "runtime-restart", "fetch", "run",
                "summarize", "generate-summary", "summary-prompt",
                "write-sinks", "plan-sinks", "subscribe", "renew-subscription",
                "delete-subscription", "maintain-subscriptions"
            ],
        }),
    };
    Ok(serde_json::to_string_pretty(&value)?)
}

fn validate_snapshot(store: &AppStore, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let config = store.config().ok();
    let teams_config = config
        .as_ref()
        .map(|config| config.teams.clone())
        .unwrap_or_else(|| json!({}));
    let graph = json!({
        "tenantId": env_present("MSGRAPH_TENANT_ID"),
        "clientId": env_present("MSGRAPH_CLIENT_ID"),
        "clientSecret": env_present("MSGRAPH_CLIENT_SECRET"),
        "webhookClientState": env_present("MSGRAPH_WEBHOOK_CLIENT_STATE"),
        "webhookStorePath": non_empty_env("MSGRAPH_WEBHOOK_STORE_PATH").is_some(),
    });
    let mut issues = Vec::<String>::new();
    let mut warnings = Vec::<String>::new();
    if !env_present("MSGRAPH_TENANT_ID")
        || !env_present("MSGRAPH_CLIENT_ID")
        || !env_present("MSGRAPH_CLIENT_SECRET")
    {
        issues.push("Microsoft Graph app-only credentials are incomplete.".into());
    }
    let delivery_mode = string_key(&teams_config, &["deliveryMode", "delivery_mode"]);
    let teams_enabled = bool_key(&teams_config, &["enabled"]);
    if !teams_enabled {
        warnings.push("Teams outbound delivery is disabled.".into());
    } else if delivery_mode == "incoming_webhook" {
        if string_key(
            &teams_config,
            &["incomingWebhookUrl", "incoming_webhook_url"],
        )
        .is_empty()
        {
            issues.push("TEAMS_INCOMING_WEBHOOK_URL is required for incoming_webhook mode.".into());
        }
    } else if delivery_mode == "graph" {
        let has_token = !string_key(&teams_config, &["accessToken", "access_token"]).is_empty();
        let has_app_credentials = env_present("MSGRAPH_TENANT_ID")
            && env_present("MSGRAPH_CLIENT_ID")
            && env_present("MSGRAPH_CLIENT_SECRET");
        if !has_token && !has_app_credentials {
            issues.push(
                "TEAMS_GRAPH_ACCESS_TOKEN or complete MSGRAPH_* app credentials is required for graph delivery mode."
                    .into(),
            );
        }
        let team_id = string_key(&teams_config, &["teamId", "team_id"]);
        let channel_id = string_key(&teams_config, &["channelId", "channel_id"]);
        let chat_id = string_key(&teams_config, &["chatId", "chat_id"]);
        let home = string_key(&teams_config, &["homeChannel", "home_channel"]);
        if team_id.is_empty() && chat_id.is_empty() {
            issues
                .push("TEAMS_TEAM_ID or TEAMS_CHAT_ID is required for graph delivery mode.".into());
        }
        if channel_id.is_empty() && chat_id.is_empty() && home.is_empty() {
            issues.push("TEAMS_CHANNEL_ID, TEAMS_CHAT_ID, or TEAMS_HOME_CHANNEL is required for graph delivery mode.".into());
        }
    } else {
        warnings.push("TEAMS_DELIVERY_MODE is not set.".into());
    }
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "plugin": "teams_pipeline",
        "kind": "standalone",
        "status": if issues.is_empty() { "configured_or_boundary" } else { "needs_configuration" },
        "ok": issues.is_empty(),
        "issues": issues,
        "warnings": warnings,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeExists": store_path.exists(),
        "storeStats": store_stats(state),
        "graphConfig": graph,
        "teamsConfig": {
            "enabled": teams_enabled,
            "deliveryMode": if delivery_mode.is_empty() { Value::Null } else { json!(delivery_mode) },
            "incomingWebhookConfigured": !string_key(&teams_config, &["incomingWebhookUrl", "incoming_webhook_url"]).is_empty(),
            "graphAccessTokenConfigured": !string_key(&teams_config, &["accessToken", "access_token"]).is_empty(),
            "teamIdConfigured": !string_key(&teams_config, &["teamId", "team_id"]).is_empty(),
            "channelIdConfigured": !string_key(&teams_config, &["channelId", "channel_id"]).is_empty(),
            "chatIdConfigured": !string_key(&teams_config, &["chatId", "chat_id"]).is_empty(),
            "homeChannelConfigured": !string_key(&teams_config, &["homeChannel", "home_channel"]).is_empty(),
        },
        "operatorCli": {
            "command": "hermes teams-pipeline",
            "actions": ["list", "show", "run", "fetch", "subscriptions", "subscribe", "renew-subscription", "delete-subscription", "maintain-subscriptions", "token-health", "validate"],
            "nativeStoreActions": ["upsert-subscription", "delete-local-subscription", "upsert-job", "upsert-sink-record", "get-sink-record", "receipt-key", "has-notification-receipt", "record-notification-receipt", "record-event-timestamp", "get-event-timestamp", "webhook-validation", "webhook-notification"],
            "modelToolsRegisteredByHermesPlugin": false,
            "agentInvocation": "Use terminal/process for Graph/network operator CLI flows; teams_pipeline also adapts Hermes TeamsPipelineStore local state mutations natively."
        },
        "runtimeBoundary": {
            "graphClient": "MicrosoftGraphTokenProvider.from_env + MicrosoftGraphClient",
            "pipelineRuntime": "TeamsMeetingPipeline plus Graph transcript/recording/call-record resolution",
            "gatewayBinding": "Hermes binds runtime to MSGRAPH_WEBHOOK adapter notification scheduler",
            "desktopAdaptation": "SynthChat exposes durable store/config/subscription diagnostics, native TeamsPipelineStore-compatible local state writes, local notification scheduling markers through schedule-received, and explicit external Graph operation boundaries instead of embedding the Python standalone plugin runtime."
        },
        "gatewaySchedulerRuntime": teams_pipeline_gateway_scheduler_runtime_contract(&teams_config, state),
        "gateway_scheduler_runtime": teams_pipeline_gateway_scheduler_runtime_contract(&teams_config, state),
        "pipelineRuntimeConfig": teams_pipeline_runtime_config_contract(&teams_config),
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state)
    })
}

fn gateway_scheduler_runtime_plan(
    store: &AppStore,
    payload: &Value,
    state: &Option<Value>,
) -> Value {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("gateway-runtime")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    let teams_config = store
        .config()
        .ok()
        .map(|config| config.teams)
        .unwrap_or_else(|| json!({}));
    let runtime = teams_pipeline_gateway_scheduler_runtime_contract(&teams_config, state);
    let execute_requested = bool_key(payload, &["execute", "live", "apply"]);
    let command_override = string_key(
        payload,
        &[
            "gatewayCommand",
            "gateway_command",
            "schedulerCommand",
            "scheduler_command",
            "command",
        ],
    );
    let mut managed_process_plan = runtime
        .get("managedProcessPlan")
        .cloned()
        .unwrap_or(Value::Null);
    let mut managed_process_plan_snake = runtime
        .get("managed_process_plan")
        .cloned()
        .unwrap_or(Value::Null);
    if !command_override.is_empty() {
        managed_process_plan["command"] = json!(command_override.clone());
        managed_process_plan["managedProcessStartPayload"]["command"] =
            json!(command_override.clone());
        managed_process_plan_snake["command"] = json!(command_override.clone());
        managed_process_plan_snake["managed_process_start_payload"]["command"] =
            json!(command_override);
    }
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "plugin": "teams_pipeline",
        "action": action,
        "status": if execute_requested { "managed_process_execution_requested" } else { "gateway_runtime_plan" },
        "executeRequested": execute_requested,
        "execute_requested": execute_requested,
        "gatewaySchedulerRuntime": runtime.clone(),
        "gateway_scheduler_runtime": runtime,
        "managedProcessPlan": managed_process_plan,
        "managed_process_plan": managed_process_plan_snake,
        "boundary": "This action exposes the Hermes Teams pipeline MSGRAPH_WEBHOOK scheduler runtime launch/stop contract directly. Starting the external Hermes gateway process should go through the async managed-process path so approvals, logs, and stop controls remain visible."
    })
}

fn teams_pipeline_gateway_scheduler_runtime_contract(
    teams_config: &Value,
    state: &Option<Value>,
) -> Value {
    let received_jobs = count_pipeline_jobs_by_status(state, &["received"]);
    let queued_jobs = count_pipeline_jobs_by_status(state, &["queued"]);
    let active_jobs = count_pipeline_jobs_by_status(
        state,
        &[
            "fetching_artifacts",
            "summarizing",
            "writing_sinks",
            "transcribing_audio",
        ],
    );
    let terminal_jobs =
        count_pipeline_jobs_by_status(state, &["completed", "failed", "retry_scheduled"]);
    let meeting_pipeline = object_key(teams_config, &["meetingPipeline", "meeting_pipeline"])
        .unwrap_or_else(|| json!({}));
    let auto_schedule = bool_key(
        &meeting_pipeline,
        &[
            "autoSchedule",
            "auto_schedule",
            "autoEnqueue",
            "auto_enqueue",
        ],
    );
    let managed_process_plan = teams_pipeline_gateway_scheduler_managed_process_plan();
    let managed_process_plan_snake = teams_pipeline_gateway_scheduler_managed_process_plan_snake();
    json!({
        "schema": "hermes_teams_pipeline_gateway_scheduler_runtime_desktop_v1",
        "hermesReferences": [
            "plugins/teams_pipeline/runtime.py::bind_gateway_runtime",
            "plugins/teams_pipeline/runtime.py::build_pipeline_runtime",
            "plugins/teams_pipeline/pipeline.py::TeamsMeetingPipeline.run_notification",
            "gateway/platforms/msgraph_webhook.py"
        ],
        "hermes_references": [
            "plugins/teams_pipeline/runtime.py::bind_gateway_runtime",
            "plugins/teams_pipeline/runtime.py::build_pipeline_runtime",
            "plugins/teams_pipeline/pipeline.py::TeamsMeetingPipeline.run_notification",
            "gateway/platforms/msgraph_webhook.py"
        ],
        "gatewayAdapter": "MSGRAPH_WEBHOOK",
        "gateway_adapter": "MSGRAPH_WEBHOOK",
        "bindGatewayRuntimePlan": true,
        "bind_gateway_runtime_plan": true,
        "nativeLongRunningSchedulerEmbedded": false,
        "native_long_running_scheduler_embedded": false,
        "dropSchedulerWhenRuntimeUnavailable": true,
        "drop_scheduler_when_runtime_unavailable": true,
        "desktopWebhookIngress": "/msgraph/webhook",
        "desktop_webhook_ingress": "/msgraph/webhook",
        "desktopScheduleReceivedAction": "teams_pipeline schedule-received",
        "desktop_schedule_received_action": "teams_pipeline schedule-received",
        "nativeStoreReceiptDedupe": true,
        "native_store_receipt_dedupe": true,
        "nativeReceivedJobPersistence": true,
        "native_received_job_persistence": true,
        "nativeAgentQueueBridge": true,
        "native_agent_queue_bridge": true,
        "enqueueAgentSupported": true,
        "enqueue_agent_supported": true,
        "autoScheduleConfigured": auto_schedule,
        "auto_schedule_configured": auto_schedule,
        "jobStatusCounts": {
            "received": received_jobs,
            "queued": queued_jobs,
            "active": active_jobs,
            "terminal": terminal_jobs
        },
        "job_status_counts": {
            "received": received_jobs,
            "queued": queued_jobs,
            "active": active_jobs,
            "terminal": terminal_jobs
        },
        "hermesSchedulerFlow": [
            "MSGRAPH_WEBHOOK adapter accepts notification",
            "adapter.set_notification_scheduler(_schedule)",
            "_schedule awaits TeamsMeetingPipeline.run_notification(notification)",
            "pipeline persists received/active/terminal job state and sink records"
        ],
        "hermes_scheduler_flow": [
            "MSGRAPH_WEBHOOK adapter accepts notification",
            "adapter.set_notification_scheduler(_schedule)",
            "_schedule awaits TeamsMeetingPipeline.run_notification(notification)",
            "pipeline persists received/active/terminal job state and sink records"
        ],
        "synthChatFlow": [
            "native /msgraph/webhook validates and deduplicates notifications",
            "teams_pipeline webhook-notification persists received jobs",
            "teams_pipeline schedule-received marks jobs queued and writes deterministic agent prompts",
            "enqueueAgent:true appends prompts into the normal SynthChat agent queue"
        ],
        "synthchat_flow": [
            "native /msgraph/webhook validates and deduplicates notifications",
            "teams_pipeline webhook-notification persists received jobs",
            "teams_pipeline schedule-received marks jobs queued and writes deterministic agent prompts",
            "enqueueAgent:true appends prompts into the normal SynthChat agent queue"
        ],
        "managedProcessPlan": managed_process_plan,
        "managed_process_plan": managed_process_plan_snake,
        "remainingBoundary": "SynthChat adapts durable store writes, webhook ingress, schedule-received queueing, native agent enqueue, and explicit run/fetch/sink execution. It does not embed Hermes' long-running Python gateway notification scheduler that awaits TeamsMeetingPipeline.run_notification inside the MSGRAPH_WEBHOOK adapter.",
        "remaining_boundary": "SynthChat adapts durable store writes, webhook ingress, schedule-received queueing, native agent enqueue, and explicit run/fetch/sink execution. It does not embed Hermes' long-running Python gateway notification scheduler that awaits TeamsMeetingPipeline.run_notification inside the MSGRAPH_WEBHOOK adapter."
    })
}

fn teams_pipeline_gateway_scheduler_managed_process_plan() -> Value {
    json!({
        "taskId": "hermes-teams-pipeline-gateway-runtime",
        "command": "hermes gateway run",
        "watchPatterns": ["Teams pipeline runtime", "MSGRAPH_WEBHOOK", "run_notification", "Dropping Graph notification"],
        "managedProcessStartPayload": {
            "action": "start",
            "label": "Hermes Teams pipeline gateway scheduler",
            "command": "hermes gateway run",
            "taskId": "hermes-teams-pipeline-gateway-runtime",
            "notifyOnComplete": true,
            "watchPatterns": ["Teams pipeline runtime", "MSGRAPH_WEBHOOK", "run_notification", "Dropping Graph notification"]
        },
        "managedProcessStopPayload": {
            "action": "stop_all",
            "taskId": "hermes-teams-pipeline-gateway-runtime",
            "forget": false
        }
    })
}

fn teams_pipeline_gateway_scheduler_managed_process_plan_snake() -> Value {
    json!({
        "task_id": "hermes-teams-pipeline-gateway-runtime",
        "command": "hermes gateway run",
        "watch_patterns": ["Teams pipeline runtime", "MSGRAPH_WEBHOOK", "run_notification", "Dropping Graph notification"],
        "managed_process_start_payload": {
            "action": "start",
            "label": "Hermes Teams pipeline gateway scheduler",
            "command": "hermes gateway run",
            "taskId": "hermes-teams-pipeline-gateway-runtime",
            "task_id": "hermes-teams-pipeline-gateway-runtime",
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["Teams pipeline runtime", "MSGRAPH_WEBHOOK", "run_notification", "Dropping Graph notification"],
            "watch_patterns": ["Teams pipeline runtime", "MSGRAPH_WEBHOOK", "run_notification", "Dropping Graph notification"]
        },
        "managed_process_stop_payload": {
            "action": "stop_all",
            "taskId": "hermes-teams-pipeline-gateway-runtime",
            "task_id": "hermes-teams-pipeline-gateway-runtime",
            "forget": false
        }
    })
}

fn count_pipeline_jobs_by_status(state: &Option<Value>, statuses: &[&str]) -> usize {
    let Some(jobs) = state
        .as_ref()
        .and_then(|value| value.get("jobs"))
        .and_then(Value::as_object)
    else {
        return 0;
    };
    jobs.values()
        .filter(|job| {
            job.get("status")
                .and_then(Value::as_str)
                .map(|status| statuses.iter().any(|expected| status == *expected))
                .unwrap_or(false)
        })
        .count()
}

fn teams_pipeline_runtime_config_contract(teams_config: &Value) -> Value {
    let pipeline_config = object_key(teams_config, &["meetingPipeline", "meeting_pipeline"])
        .unwrap_or_else(|| json!({}));
    let teams_delivery_config = object_key(&pipeline_config, &["teamsDelivery", "teams_delivery"])
        .unwrap_or_else(|| json!({}));
    let delivery_mode = string_key(&teams_delivery_config, &["mode", "delivery_mode"])
        .if_empty_then(|| string_key(teams_config, &["deliveryMode", "delivery_mode"]));
    let incoming_webhook_url = string_key(
        &teams_delivery_config,
        &["incomingWebhookUrl", "incoming_webhook_url"],
    )
    .if_empty_then(|| {
        string_key(
            teams_config,
            &["incomingWebhookUrl", "incoming_webhook_url"],
        )
    });
    let access_token = string_key(&teams_delivery_config, &["accessToken", "access_token"])
        .if_empty_then(|| string_key(teams_config, &["accessToken", "access_token"]));
    let team_id = string_key(&teams_delivery_config, &["teamId", "team_id"])
        .if_empty_then(|| string_key(teams_config, &["teamId", "team_id"]));
    let channel_id = string_key(&teams_delivery_config, &["channelId", "channel_id"])
        .if_empty_then(|| string_key(teams_config, &["channelId", "channel_id"]));
    let chat_id = string_key(&teams_delivery_config, &["chatId", "chat_id"])
        .if_empty_then(|| string_key(teams_config, &["chatId", "chat_id"]));
    let delivery_enabled = match delivery_mode.as_str() {
        "incoming_webhook" => !incoming_webhook_url.is_empty(),
        "graph" => !chat_id.is_empty() || (!team_id.is_empty() && !channel_id.is_empty()),
        _ => false,
    };
    json!({
        "schema": "hermes_teams_pipeline_runtime_config_desktop_v1",
        "source": {
            "hermes": "gateway.config Platform('teams').extra.meeting_pipeline plus Teams platform delivery fields",
            "synthChat": "settings.teams meetingPipeline/meeting_pipeline plus top-level Teams delivery fields",
        },
        "defaults": {
            "transcriptPreferred": true,
            "transcriptRequired": false,
            "transcriptionFallback": true,
            "ffmpegExtractAudio": true,
            "transcriptMinChars": 80
        },
        "effective": {
            "transcriptPreferred": bool_key_opt(&pipeline_config, &["transcriptPreferred", "transcript_preferred"]).unwrap_or(true),
            "transcriptRequired": bool_key_opt(&pipeline_config, &["transcriptRequired", "transcript_required"]).unwrap_or(false),
            "transcriptionFallback": bool_key_opt(&pipeline_config, &["transcriptionFallback", "transcription_fallback"]).unwrap_or(true),
            "sttModel": string_key(&pipeline_config, &["sttModel", "stt_model"]),
            "ffmpegExtractAudio": bool_key_opt(&pipeline_config, &["ffmpegExtractAudio", "ffmpeg_extract_audio"]).unwrap_or(true),
            "transcriptMinChars": int_key(&pipeline_config, &["transcriptMinChars", "transcript_min_chars"], 80),
        },
        "sinks": {
            "notion": {
                "configured": object_key(&pipeline_config, &["notion"]).is_some(),
                "enabled": object_key(&pipeline_config, &["notion"]).as_ref().and_then(|value| bool_key_opt(value, &["enabled"])).unwrap_or(false),
                "envReady": env_present("NOTION_API_KEY")
            },
            "linear": {
                "configured": object_key(&pipeline_config, &["linear"]).is_some(),
                "enabled": object_key(&pipeline_config, &["linear"]).as_ref().and_then(|value| bool_key_opt(value, &["enabled"])).unwrap_or(false),
                "envReady": env_present("LINEAR_API_KEY")
            },
            "teamsDelivery": {
                "mode": if delivery_mode.is_empty() { Value::Null } else { json!(delivery_mode) },
                "enabled": delivery_enabled,
                "incomingWebhookConfigured": !incoming_webhook_url.is_empty(),
                "accessTokenConfigured": !access_token.is_empty(),
                "teamIdConfigured": !team_id.is_empty(),
                "channelIdConfigured": !channel_id.is_empty(),
                "chatIdConfigured": !chat_id.is_empty(),
                "writer": "TeamsSummaryWriter when Teams platform is enabled and delivery target is configured"
            }
        },
        "gatewayBinding": {
            "adapter": "MSGRAPH_WEBHOOK",
            "runtimeBuilder": "build_pipeline_runtime(gateway)",
            "unavailableBehavior": "Hermes installs a drop scheduler that acknowledges Graph notifications without piling up unbound work"
        }
    })
}

fn teams_graph_runtime_contract(store_path: &PathBuf, state: &Option<Value>) -> Value {
    json!({
        "schema": "hermes_teams_pipeline_graph_runtime_contract_desktop_v1",
        "graphClient": {
            "provider": "MicrosoftGraphTokenProvider.from_env",
            "client": "MicrosoftGraphClient",
            "baseUrl": "https://graph.microsoft.com/v1.0",
            "requiredEnv": ["MSGRAPH_TENANT_ID", "MSGRAPH_CLIENT_ID", "MSGRAPH_CLIENT_SECRET"],
            "configured": env_present("MSGRAPH_TENANT_ID") && env_present("MSGRAPH_CLIENT_ID") && env_present("MSGRAPH_CLIENT_SECRET"),
        },
        "operatorCli": {
            "command": "hermes teams-pipeline",
            "actions": {
                "fetch": "resolve meeting reference, transcript, recordings, and call-record artifact metadata",
                "run": "TeamsMeetingPipeline.run_job(job_id)",
                "subscriptions": "GET /subscriptions and sync active records into TeamsPipelineStore",
                "subscribe": "POST /subscriptions",
                "renewSubscription": "PATCH /subscriptions/{subscription_id}",
                "deleteSubscription": "DELETE /subscriptions/{subscription_id}",
                "maintainSubscriptions": "GET /subscriptions, sync managed records, renew near-expiry managed subscriptions, mark local records missing_remote",
                "tokenHealth": "inspect or force-refresh Microsoft Graph app-only token"
            }
        },
        "subscriptionEndpoints": {
            "list": {"method": "GET", "path": "/subscriptions"},
            "create": {"method": "POST", "path": "/subscriptions"},
            "renew": {"method": "PATCH", "path": "/subscriptions/{subscription_id}", "body": {"expirationDateTime": "<ISO-8601 UTC>"}},
            "delete": {"method": "DELETE", "path": "/subscriptions/{subscription_id}"}
        },
        "meetingArtifactEndpoints": {
            "meetingLookup": [
                {"method": "GET", "path": "/communications/onlineMeetings/{meeting_id}"},
                {"method": "GET", "path": "/communications/onlineMeetings?$filter=JoinWebUrl eq '{join_web_url}'"}
            ],
            "transcripts": {"method": "GET", "path": "/communications/onlineMeetings/{meeting_id}/transcripts"},
            "recordings": {"method": "GET", "path": "/communications/onlineMeetings/{meeting_id}/recordings"},
            "callRecords": {"method": "GET", "path": "/communications/callRecords/{call_record_id}"}
        },
        "webhookRuntime": {
            "ingress": "/msgraph/webhook",
            "validation": "echo validationToken as text/plain",
            "notificationBatch": "accept value[] notifications after clientState/resource checks",
            "receiptDedupe": "TeamsPipelineStore.notification_receipts",
            "schedulerBinding": "Hermes MSGRAPH_WEBHOOK adapter schedules TeamsMeetingPipeline.run_notification(notification); SynthChat records received jobs, can locally mark them queued through schedule-received, and exposes native API ingress."
        },
        "store": {
            "path": store_path.to_string_lossy().to_string(),
            "stats": store_stats(state)
        },
        "desktopExecutionBoundary": "SynthChat previews Hermes Graph request shapes and performs local store-compatible mutations only; live Graph calls and TeamsMeetingPipeline replay remain terminal/process-owned operator flows."
    })
}

fn list_jobs(payload: &Value, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut jobs = object_entries(state, "jobs")
        .into_iter()
        .filter(|job| {
            status.is_empty()
                || job
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|value| value.eq_ignore_ascii_case(&status))
                    .unwrap_or(false)
        })
        .map(compact_job)
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| {
        str_field(right, "updated_at")
            .cmp(&str_field(left, "updated_at"))
            .then(str_field(right, "updatedAt").cmp(&str_field(left, "updatedAt")))
    });
    jobs.truncate(limit);
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "list",
        "storePath": store_path.to_string_lossy().to_string(),
        "count": jobs.len(),
        "jobs": jobs,
    })
}

fn show_job(payload: &Value, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let job_id = string_key(payload, &["jobId", "job_id", "id"]);
    let job = state
        .as_ref()
        .and_then(|state| state.get("jobs"))
        .and_then(Value::as_object)
        .and_then(|jobs| jobs.get(&job_id))
        .cloned()
        .map(compact_job);
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "show",
        "storePath": store_path.to_string_lossy().to_string(),
        "jobId": job_id,
        "found": job.is_some(),
        "job": job,
    })
}

fn list_subscriptions(store_path: &PathBuf, state: &Option<Value>) -> Value {
    let subscriptions = object_entries(state, "subscriptions");
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "subscriptions",
        "storePath": store_path.to_string_lossy().to_string(),
        "count": subscriptions.len(),
        "subscriptions": subscriptions,
    })
}

fn upsert_subscription(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let mut record = payload
        .get("subscription")
        .or_else(|| payload.get("record"))
        .or_else(|| payload.get("payload"))
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| payload.clone());
    let subscription_id = string_key(
        &record,
        &["subscription_id", "subscriptionId", "id", "subscriptionID"],
    );
    if subscription_id.is_empty() {
        return Err(AppError::BadRequest(
            "subscription_id or id is required".into(),
        ));
    }
    normalize_subscription_record(&mut record, &subscription_id);
    let mut state = ensure_store_state(state);
    let saved = upsert_object_record(
        &mut state,
        "subscriptions",
        &subscription_id,
        record,
        "subscription_id",
    );
    write_store_state(store_path, &state)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "upsert-subscription",
        "storePath": store_path.to_string_lossy().to_string(),
        "subscriptionId": subscription_id,
        "subscription": saved,
        "storeStats": store_stats(&Some(state)),
    }))
}

fn delete_local_subscription(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let subscription_id = string_key(payload, &["subscriptionId", "subscription_id", "id"]);
    if subscription_id.is_empty() {
        return Err(AppError::BadRequest("subscription_id is required".into()));
    }
    let mut state = ensure_store_state(state);
    let removed = state
        .get_mut("subscriptions")
        .and_then(Value::as_object_mut)
        .and_then(|subscriptions| subscriptions.remove(&subscription_id))
        .is_some();
    if removed {
        write_store_state(store_path, &state)?;
    }
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "delete-local-subscription",
        "storePath": store_path.to_string_lossy().to_string(),
        "subscriptionId": subscription_id,
        "removed": removed,
        "storeStats": store_stats(&Some(state)),
    }))
}

fn upsert_job(payload: &Value, store_path: &PathBuf, state: Option<Value>) -> AppResult<Value> {
    let record = payload
        .get("job")
        .or_else(|| payload.get("record"))
        .or_else(|| payload.get("payload"))
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| payload.clone());
    let job_id = string_key(&record, &["job_id", "jobId", "id"]);
    if job_id.is_empty() {
        return Err(AppError::BadRequest("job_id is required".into()));
    }
    let mut state = ensure_store_state(state);
    let saved = upsert_object_record(&mut state, "jobs", &job_id, record, "job_id");
    write_store_state(store_path, &state)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "upsert-job",
        "storePath": store_path.to_string_lossy().to_string(),
        "jobId": job_id,
        "job": compact_job(saved),
        "storeStats": store_stats(&Some(state)),
    }))
}

fn upsert_sink_record(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let record = payload
        .get("sinkRecord")
        .or_else(|| payload.get("sink_record"))
        .or_else(|| payload.get("record"))
        .or_else(|| payload.get("payload"))
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| payload.clone());
    let sink_key = string_key(&record, &["sink_key", "sinkKey", "key", "id"]);
    if sink_key.is_empty() {
        return Err(AppError::BadRequest("sink_key is required".into()));
    }
    let mut state = ensure_store_state(state);
    let saved = upsert_object_record(&mut state, "sink_records", &sink_key, record, "sink_key");
    write_store_state(store_path, &state)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "upsert-sink-record",
        "storePath": store_path.to_string_lossy().to_string(),
        "sinkKey": sink_key,
        "sinkRecord": saved,
        "storeStats": store_stats(&Some(state)),
    }))
}

fn get_sink_record(payload: &Value, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let sink_key = string_key(payload, &["sinkKey", "sink_key", "key", "id"]);
    let record = state
        .as_ref()
        .and_then(|state| state.get("sink_records"))
        .and_then(Value::as_object)
        .and_then(|records| records.get(&sink_key))
        .cloned();
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "get-sink-record",
        "storePath": store_path.to_string_lossy().to_string(),
        "sinkKey": sink_key,
        "found": record.is_some(),
        "sinkRecord": record,
    })
}

fn notification_receipt_key(payload: &Value) -> Value {
    let notification = payload
        .get("notification")
        .or_else(|| payload.get("payload"))
        .cloned()
        .unwrap_or_else(|| payload.clone());
    let receipt_key = build_notification_receipt_key(&notification);
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "receipt-key",
        "receiptKey": receipt_key,
    })
}

fn has_notification_receipt(payload: &Value, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let receipt_key = receipt_key_from_payload(payload);
    let present = state
        .as_ref()
        .and_then(|state| state.get("notification_receipts"))
        .and_then(Value::as_object)
        .map(|receipts| receipts.contains_key(&receipt_key))
        .unwrap_or(false);
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "has-notification-receipt",
        "storePath": store_path.to_string_lossy().to_string(),
        "receiptKey": receipt_key,
        "present": present,
    })
}

fn record_notification_receipt(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let receipt_key = receipt_key_from_payload(payload);
    let receipt_payload = payload
        .get("receiptPayload")
        .or_else(|| payload.get("payload"))
        .or_else(|| payload.get("notification"))
        .cloned()
        .unwrap_or(Value::Null);
    let received_at = string_key(payload, &["receivedAt", "received_at"]).if_empty_then(now_iso);
    let mut state = ensure_store_state(state);
    let already_present = state
        .get("notification_receipts")
        .and_then(Value::as_object)
        .map(|receipts| receipts.contains_key(&receipt_key))
        .unwrap_or(false);
    if !already_present {
        let receipts = state
            .get_mut("notification_receipts")
            .and_then(Value::as_object_mut)
            .expect("store state initializes notification_receipts");
        receipts.insert(
            receipt_key.clone(),
            json!({"received_at": received_at, "payload": receipt_payload}),
        );
        write_store_state(store_path, &state)?;
    }
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "record-notification-receipt",
        "storePath": store_path.to_string_lossy().to_string(),
        "receiptKey": receipt_key,
        "recorded": !already_present,
        "storeStats": store_stats(&Some(state)),
    }))
}

fn record_event_timestamp(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let event_key = string_key(payload, &["eventKey", "event_key", "key"]);
    if event_key.is_empty() {
        return Err(AppError::BadRequest("event_key is required".into()));
    }
    let timestamp = string_key(payload, &["timestamp", "time"]).if_empty_then(now_iso);
    let mut state = ensure_store_state(state);
    let events = state
        .get_mut("event_timestamps")
        .and_then(Value::as_object_mut)
        .expect("store state initializes event_timestamps");
    events.insert(event_key.clone(), json!(timestamp));
    write_store_state(store_path, &state)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "record-event-timestamp",
        "storePath": store_path.to_string_lossy().to_string(),
        "eventKey": event_key,
        "timestamp": timestamp,
        "storeStats": store_stats(&Some(state)),
    }))
}

fn get_event_timestamp(payload: &Value, store_path: &PathBuf, state: &Option<Value>) -> Value {
    let event_key = string_key(payload, &["eventKey", "event_key", "key"]);
    let timestamp = state
        .as_ref()
        .and_then(|state| state.get("event_timestamps"))
        .and_then(Value::as_object)
        .and_then(|events| events.get(&event_key))
        .and_then(Value::as_str)
        .map(str::to_string);
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "get-event-timestamp",
        "storePath": store_path.to_string_lossy().to_string(),
        "eventKey": event_key,
        "found": timestamp.is_some(),
        "timestamp": timestamp,
    })
}

fn webhook_validation(payload: &Value) -> Value {
    let token = string_key(
        payload,
        &[
            "validationToken",
            "validation_token",
            "token",
            "query.validationToken",
        ],
    );
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "webhook-validation",
        "status": if token.is_empty() { "bad_request" } else { "ok" },
        "responseStatus": if token.is_empty() { 400 } else { 200 },
        "contentType": "text/plain",
        "body": token,
        "boundary": "Hermes MSGraphWebhookAdapter echoes validationToken from GET/POST. SynthChat exposes the same handshake shape through this native teams_pipeline action; HTTP routing remains the desktop API/gateway layer.",
    })
}

fn process_webhook_notification(
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let validation_token = string_key(payload, &["validationToken", "validation_token", "token"]);
    if !validation_token.is_empty() {
        return Ok(webhook_validation(
            &json!({"validationToken": validation_token}),
        ));
    }

    let notifications = webhook_notifications(payload);
    if notifications.is_none() {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "webhook-notification",
            "status": "bad_request",
            "responseStatus": 400,
            "reason": "payload.value must be an array or notification must be an object",
            "storePath": store_path.to_string_lossy().to_string(),
        }));
    }
    let notifications = notifications.unwrap();
    let mut state = ensure_store_state(state);
    let mut accepted = Vec::<Value>::new();
    let mut duplicate_count = 0usize;
    let mut auth_rejected = 0usize;
    let mut resource_rejected = 0usize;
    let mut malformed = 0usize;

    for notification in notifications {
        if !notification.is_object() {
            malformed += 1;
            continue;
        }
        if !webhook_resource_allowed(&notification, payload) {
            resource_rejected += 1;
            continue;
        }
        if !webhook_client_state_allowed(&notification, payload) {
            auth_rejected += 1;
            continue;
        }
        let receipt_key = build_notification_receipt_key(&notification);
        if store_contains_key(&state, "notification_receipts", &receipt_key) {
            duplicate_count += 1;
            continue;
        }
        record_receipt_in_state(&mut state, &receipt_key, &notification, now_iso());
        let job = create_job_from_notification(&mut state, &notification, &receipt_key);
        accepted.push(json!({
            "receiptKey": receipt_key,
            "jobId": job.get("job_id").cloned().unwrap_or(Value::Null),
            "resource": notification.get("resource").cloned().unwrap_or(Value::Null),
            "changeType": notification.get("changeType").or_else(|| notification.get("change_type")).cloned().unwrap_or(Value::Null),
        }));
    }

    if !accepted.is_empty() || duplicate_count > 0 {
        write_store_state(store_path, &state)?;
    }
    let response_status = if !accepted.is_empty() || duplicate_count > 0 {
        202
    } else if auth_rejected > 0 && resource_rejected == 0 && malformed == 0 {
        403
    } else {
        400
    };
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "webhook-notification",
        "status": if response_status == 202 { "accepted" } else if response_status == 403 { "forbidden" } else { "bad_request" },
        "responseStatus": response_status,
        "accepted": accepted.len(),
        "duplicates": duplicate_count,
        "authRejected": auth_rejected,
        "resourceRejected": resource_rejected,
        "malformed": malformed,
        "acceptedNotifications": accepted,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(&Some(state)),
        "runtimeBoundary": {
            "scheduler": "Hermes calls TeamsMeetingPipeline.run_notification(notification) after adapter acceptance.",
            "desktopAdaptation": "SynthChat validates/deduplicates the webhook batch and records received pipeline jobs in the Hermes store layout; Graph artifact resolution and summary/sink execution remain the run/fetch runtime boundary."
        }
    }))
}

fn schedule_received_jobs(
    store: &AppStore,
    payload: &Value,
    store_path: &PathBuf,
    state: Option<Value>,
) -> AppResult<Value> {
    let mut state_value = ensure_store_state(state);
    let dry_run = bool_key(payload, &["dryRun", "dry_run"]);
    let enqueue_agent = bool_key(payload, &["enqueueAgent", "enqueue_agent"]);
    let include_retry = bool_key(
        payload,
        &["includeRetryScheduled", "include_retry_scheduled"],
    );
    let job_filter = string_list_key(payload, &["jobIds", "job_ids", "jobId", "job_id"])
        .map(|items| items.into_iter().collect::<HashSet<_>>())
        .unwrap_or_default();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let now = now_iso();
    let mut candidates = state_value
        .get("jobs")
        .and_then(Value::as_object)
        .map(|jobs| {
            jobs.iter()
                .filter_map(|(job_id, job)| {
                    if !job_filter.is_empty() && !job_filter.contains(job_id) {
                        return None;
                    }
                    let status = string_key(job, &["status"]).to_ascii_lowercase();
                    if status == "received" || (include_retry && status == "retry_scheduled") {
                        Some((job_id.clone(), job.clone()))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.sort_by(|(_, left), (_, right)| {
        str_field(left, "created_at")
            .cmp(&str_field(right, "created_at"))
            .then(str_field(left, "updated_at").cmp(&str_field(right, "updated_at")))
    });
    candidates.truncate(limit);

    let queue_target = if enqueue_agent && !dry_run && !candidates.is_empty() {
        Some(resolve_teams_pipeline_queue_target(store, payload)?)
    } else {
        None
    };
    let mut scheduled = Vec::<Value>::new();
    for (job_id, job) in candidates {
        let prompt = teams_pipeline_scheduler_prompt(&job_id, &job, payload);
        let queued_agent =
            if let Some((conversation_id, persona_id, queue_target_info)) = queue_target.as_ref() {
                let mut message = ChatMessage::new(
                    conversation_id.clone(),
                    "user",
                    prompt.clone(),
                    "teams_pipeline",
                );
                message.provider_data = Some(json!({
                    "source": "teams_pipeline.schedule_received",
                    "jobId": job_id,
                    "storePath": store_path.to_string_lossy().to_string(),
                    "queueTarget": queue_target_info,
                }));
                let message = store.append_message(message)?;
                let queued = store.enqueue_agent_request(
                    conversation_id.clone(),
                    persona_id.clone(),
                    &message,
                )?;
                Some(json!({
                    "id": queued.id,
                    "status": queued.status,
                    "conversationId": queued.conversation_id,
                    "personaId": queued.persona_id,
                    "userMessageId": queued.user_message_id,
                }))
            } else {
                None
            };
        let meeting_ref = job
            .get("meeting_ref")
            .or_else(|| job.get("meetingRef"))
            .cloned()
            .unwrap_or(Value::Null);
        let scheduler_record = json!({
            "status": if dry_run { "would_queue" } else { "queued" },
            "scheduled_at": now,
            "source": "synthchat-teams-pipeline-schedule-received",
            "prompt": prompt.clone(),
            "enqueue_agent": enqueue_agent,
            "queued_agent": queued_agent.clone(),
        });
        scheduled.push(json!({
            "jobId": job_id,
            "previousStatus": string_key(&job, &["status"]),
            "nextStatus": if dry_run { "received" } else { "queued" },
            "meetingRef": meeting_ref,
            "prompt": prompt.clone(),
            "queuedAgent": queued_agent,
        }));
        if !dry_run {
            upsert_object_record(
                &mut state_value,
                "jobs",
                &job_id,
                json!({
                    "status": "queued",
                    "scheduler": scheduler_record,
                    "agent_prompt": prompt,
                    "queued_agent_id": scheduled
                        .last()
                        .and_then(|item| item.get("queuedAgent"))
                        .and_then(|item| item.get("id"))
                        .cloned()
                        .unwrap_or(Value::Null),
                }),
                "job_id",
            );
        }
    }

    if !dry_run && !scheduled.is_empty() {
        let scheduler = state_value
            .as_object_mut()
            .expect("store state is an object");
        scheduler.insert(
            "scheduler".into(),
            json!({
                "last_scheduled_at": now,
                "last_scheduled_count": scheduled.len(),
                "source": "synthchat-teams-pipeline-schedule-received",
            }),
        );
        write_store_state(store_path, &state_value)?;
    }

    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "schedule-received",
        "status": if dry_run { "dry_run" } else { "queued" },
        "dryRun": dry_run,
        "enqueueAgent": enqueue_agent,
        "storePath": store_path.to_string_lossy().to_string(),
        "scheduled": scheduled.len(),
        "jobs": scheduled,
        "storeStats": store_stats(&Some(state_value)),
        "runtimeBridge": {
            "hermes": "MSGRAPH_WEBHOOK adapter schedules TeamsMeetingPipeline.run_notification(notification).",
            "synthChat": "schedule-received marks received Teams pipeline jobs as queued and writes a deterministic agent prompt; with enqueueAgent:true it also appends that prompt to a SynthChat conversation and native agent queue without starting the run from this tool.",
            "liveGraphExecuted": false
        },
        "timestamp": now_iso(),
    }))
}

fn resolve_teams_pipeline_queue_target(
    store: &AppStore,
    payload: &Value,
) -> AppResult<(String, String, Value)> {
    let requested_conversation_id = string_key(payload, &["conversationId", "conversation_id"]);
    let requested_persona_id = string_key(payload, &["personaId", "persona_id"]);
    if requested_conversation_id.is_empty() {
        let title = string_key(
            payload,
            &["conversationTitle", "conversation_title", "title"],
        );
        let conversation = store.create_conversation(
            Some(if title.is_empty() {
                "Teams pipeline scheduler".into()
            } else {
                title
            }),
            if requested_persona_id.is_empty() {
                None
            } else {
                Some(requested_persona_id)
            },
        )?;
        let persona_id = conversation
            .persona_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| AppError::BadRequest("created conversation has no persona_id".into()))?;
        return Ok((
            conversation.id.clone(),
            persona_id,
            json!({
                "createdConversation": true,
                "conversationId": conversation.id,
                "title": conversation.title,
            }),
        ));
    }

    let conversation = store.conversation(&requested_conversation_id)?;
    let persona_id = if requested_persona_id.is_empty() {
        conversation
            .persona_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "conversation {} has no persona_id; pass personaId",
                    conversation.id
                ))
            })?
    } else {
        requested_persona_id
    };
    Ok((
        conversation.id.clone(),
        persona_id,
        json!({
            "createdConversation": false,
            "conversationId": conversation.id,
            "title": conversation.title,
        }),
    ))
}

fn teams_pipeline_scheduler_prompt(job_id: &str, job: &Value, payload: &Value) -> String {
    let summarize = bool_key(
        payload,
        &[
            "summarizeWithLlm",
            "summarize_with_llm",
            "useConfiguredLlmSummary",
            "use_configured_llm_summary",
        ],
    );
    let sink_writes = bool_key(payload, &["confirmSinkWrites", "confirm_sink_writes"]);
    let meeting_ref = job
        .get("meeting_ref")
        .or_else(|| job.get("meetingRef"))
        .cloned()
        .unwrap_or(Value::Null);
    let meeting_id = string_key(&meeting_ref, &["meeting_id", "meetingId", "id"]);
    let join_web_url = string_key(&meeting_ref, &["join_web_url", "joinWebUrl"]);
    format!(
        "Run the Hermes Teams meeting pipeline job `{job_id}` using the `teams_pipeline` tool. \
First inspect the job, then execute `teams_pipeline` with action `run`, jobId `{job_id}`, \
execute true, confirmPipelineRun true, and confirmLiveGraphRead true. \
summarizeWithLlm={summarize}; confirmSinkWrites={sink_writes}. \
Meeting id: {}; join URL: {}. \
Do not print secrets; report the final pipeline status and any retry/sink errors.",
        if meeting_id.is_empty() {
            "-"
        } else {
            &meeting_id
        },
        if join_web_url.is_empty() {
            "-"
        } else {
            &join_web_url
        },
    )
}

fn token_health_snapshot(payload: &Value) -> Value {
    let configured = env_present("MSGRAPH_TENANT_ID")
        && env_present("MSGRAPH_CLIENT_ID")
        && env_present("MSGRAPH_CLIENT_SECRET");
    let force_refresh = bool_key(payload, &["forceRefresh", "force_refresh", "refresh"]);
    let execute = bool_key(payload, &["execute", "run", "start"]);
    let mut args: Vec<String> = vec!["teams-pipeline".into(), "token-health".into()];
    if force_refresh {
        args.push("--force-refresh".into());
    }
    let operator_command = format!("hermes {}", args.join(" "));
    let status = if execute {
        "managed_process_execution_requested"
    } else if force_refresh {
        "token_health_force_refresh_plan"
    } else if configured {
        "token_health_configured"
    } else {
        "token_health_needs_configuration"
    };
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "token-health",
        "status": status,
        "configured": configured,
        "forceRefreshRequested": force_refresh,
        "force_refresh_requested": force_refresh,
        "executeRequested": execute,
        "execute_requested": execute,
        "graphConfig": {
            "tenantId": env_present("MSGRAPH_TENANT_ID"),
            "clientId": env_present("MSGRAPH_CLIENT_ID"),
            "clientSecret": env_present("MSGRAPH_CLIENT_SECRET"),
        },
        "hermesCli": {
            "command": operator_command,
            "args": args,
            "forceRefresh": force_refresh,
            "printsJson": true
        },
        "operatorCommand": operator_command,
        "operator_command": operator_command,
        "managedProcessStartPayload": {
            "taskId": "hermes-teams-pipeline-token-health",
            "label": "Hermes Teams pipeline token health",
            "command": operator_command,
            "cwd": std::env::current_dir().map(|path| path.to_string_lossy().to_string()).unwrap_or_else(|_| ".".into()),
            "env": {
                "MSGRAPH_TENANT_ID": env_present("MSGRAPH_TENANT_ID"),
                "MSGRAPH_CLIENT_ID": env_present("MSGRAPH_CLIENT_ID"),
                "MSGRAPH_CLIENT_SECRET": env_present("MSGRAPH_CLIENT_SECRET")
            }
        },
        "managed_process_start_payload": {
            "taskId": "hermes-teams-pipeline-token-health",
            "label": "Hermes Teams pipeline token health",
            "command": operator_command,
            "cwd": std::env::current_dir().map(|path| path.to_string_lossy().to_string()).unwrap_or_else(|_| ".".into()),
            "env": {
                "MSGRAPH_TENANT_ID": env_present("MSGRAPH_TENANT_ID"),
                "MSGRAPH_CLIENT_ID": env_present("MSGRAPH_CLIENT_ID"),
                "MSGRAPH_CLIENT_SECRET": env_present("MSGRAPH_CLIENT_SECRET")
            }
        },
        "forceRefreshSupportedHere": false,
        "forceRefreshExecutionPath": "managed_process",
        "force_refresh_execution_path": "managed_process",
        "boundary": "Hermes token-health calls MicrosoftGraphTokenProvider.from_env and can force-refresh the token. SynthChat reports offline readiness and returns the exact Hermes CLI managed-process plan so live refresh runs through normal approvals, logs, stop controls, and secret-safe environment handling instead of performing token network requests inside this diagnostic tool.",
    })
}

fn plan_graph_fetch(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let meeting_id = string_key(payload, &["meetingId", "meeting_id"]);
    let join_web_url = string_key(payload, &["joinWebUrl", "join_web_url"]);
    let tenant_id = string_key(payload, &["tenantId", "tenant_id"]);
    let call_record_id = string_key(payload, &["callRecordId", "call_record_id"]);
    if meeting_id.is_empty() && join_web_url.is_empty() {
        return Err(AppError::BadRequest(
            "fetch requires meetingId or joinWebUrl".into(),
        ));
    }
    let resolved_meeting_token = if meeting_id.is_empty() {
        "{resolved_meeting_id}".into()
    } else {
        percent_encode_graph_path_segment(&meeting_id)
    };
    let mut requests = Vec::<Value>::new();
    if !meeting_id.is_empty() {
        requests.push(json!({
            "stage": "resolve_meeting_reference",
            "method": "GET",
            "path": format!("/communications/onlineMeetings/{resolved_meeting_token}")
        }));
    } else {
        let escaped_join_url = join_web_url.replace('\'', "''");
        requests.push(json!({
            "stage": "resolve_meeting_reference",
            "method": "GET",
            "path": "/communications/onlineMeetings",
            "query": {"$filter": format!("JoinWebUrl eq '{escaped_join_url}'")}
        }));
    }
    requests.push(json!({
        "stage": "list_transcript_artifacts",
        "method": "GET",
        "path": format!("/communications/onlineMeetings/{resolved_meeting_token}/transcripts")
    }));
    requests.push(json!({
        "stage": "download_preferred_transcript",
        "method": "GET",
        "path": format!("/communications/onlineMeetings/{resolved_meeting_token}/transcripts/{{transcript_id}}/content"),
        "fallback": "@microsoft.graph.downloadUrl when present"
    }));
    requests.push(json!({
        "stage": "list_recording_artifacts",
        "method": "GET",
        "path": format!("/communications/onlineMeetings/{resolved_meeting_token}/recordings")
    }));
    if !call_record_id.is_empty() {
        requests.push(json!({
            "stage": "fetch_call_record_artifact",
            "method": "GET",
            "path": format!("/communications/callRecords/{}", percent_encode_graph_path_segment(&call_record_id)),
            "permissionErrors": "Hermes treats 401/403 as optional enrichment miss by default"
        }));
    }
    let plan = json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "fetch",
        "status": "graph_artifact_resolution_plan",
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(state),
        "meetingRefInput": {
            "meeting_id": if meeting_id.is_empty() { Value::Null } else { json!(meeting_id) },
            "join_web_url": if join_web_url.is_empty() { Value::Null } else { json!(join_web_url) },
            "tenant_id": if tenant_id.is_empty() { Value::Null } else { json!(tenant_id) },
            "call_record_id": if call_record_id.is_empty() { Value::Null } else { json!(call_record_id) },
        },
        "graphRequests": requests,
        "artifactSelection": {
            "transcript": "Hermes selects the preferred transcript by completed/available status, downloadable source, and latest timestamp.",
            "recordings": "Hermes lists recordings and returns up to five artifacts in CLI dry-run output.",
            "callRecord": "Hermes optionally enriches with communications/callRecords when call_record_id is supplied or present in meeting metadata."
        },
        "operatorCommand": "hermes teams-pipeline fetch",
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "desktopExecutionBoundary": "SynthChat returns the Hermes fetch/test Graph plan without issuing network requests. Execute the operator CLI through the normal terminal/process path for live artifact resolution.",
        "timestamp": now_iso(),
    });
    if graph_live_requested(payload) {
        execute_graph_fetch(payload, store_path, state, plan)
    } else {
        Ok(plan)
    }
}

fn execute_graph_fetch(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
    plan: Value,
) -> AppResult<Value> {
    if !bool_key(
        payload,
        &[
            "confirmLiveGraphRead",
            "confirm_live_graph_read",
            "confirmLiveGraphMutation",
            "confirm_live_graph_mutation",
        ],
    ) {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "fetch",
            "status": "live_confirmation_required",
            "executed": false,
            "requiredFlag": "confirmLiveGraphRead:true",
            "reason": "Live Microsoft Graph meeting artifact reads can expose meeting transcripts, recordings, and call metadata; they require explicit confirmation.",
            "planned": plan,
            "timestamp": now_iso(),
        }));
    }

    let meeting_id = string_key(payload, &["meetingId", "meeting_id"]);
    let join_web_url = string_key(payload, &["joinWebUrl", "join_web_url"]);
    let tenant_id = string_key(payload, &["tenantId", "tenant_id"]);
    let call_record_id = string_key(payload, &["callRecordId", "call_record_id"]);
    let token = microsoft_graph_access_token()?;
    let base_url = non_empty_env("MSGRAPH_GRAPH_BASE_URL")
        .or_else(|| non_empty_env("MICROSOFT_GRAPH_BASE_URL"))
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into());
    let client = graph_http_client()?;

    let (meeting_ref, meeting_payload) = if !meeting_id.is_empty() {
        let path = format!(
            "/communications/onlineMeetings/{}",
            percent_encode_graph_path_segment(&meeting_id)
        );
        let payload = graph_json_request(&client, &token, "GET", &base_url, &path, None)?;
        let meeting_ref = graph_meeting_ref_from_payload(&payload, &tenant_id);
        (meeting_ref, payload)
    } else {
        let escaped_join_url = join_web_url.replace('\'', "''");
        let filter = format!("JoinWebUrl eq '{escaped_join_url}'");
        let path = format!(
            "/communications/onlineMeetings?%24filter={}",
            percent_encode_query_component(&filter)
        );
        let payload = graph_json_request(&client, &token, "GET", &base_url, &path, None)?;
        let meeting = payload
            .get("value")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Teams meeting not found for join URL: {join_web_url}"
                ))
            })?;
        let meeting_ref = graph_meeting_ref_from_payload(&meeting, &tenant_id);
        (meeting_ref, meeting)
    };

    let resolved_meeting_id = string_key(&meeting_ref, &["meeting_id"]);
    if resolved_meeting_id.is_empty() {
        return Err(AppError::BadRequest(
            "Microsoft Graph meeting response missing id".into(),
        ));
    }
    let meeting_token = percent_encode_graph_path_segment(&resolved_meeting_id);
    let transcript_path = format!("/communications/onlineMeetings/{meeting_token}/transcripts");
    let transcript_payloads =
        collect_graph_paginated_values(&client, &token, &base_url, &transcript_path)?;
    let transcripts = transcript_payloads
        .iter()
        .map(|payload| normalize_graph_artifact("transcript", payload))
        .collect::<Vec<_>>();
    let preferred_transcript = select_preferred_graph_transcript(&transcripts);
    let transcript_text = preferred_transcript.as_ref().and_then(|artifact| {
        graph_artifact_download_path(&meeting_token, artifact, "transcripts")
            .and_then(|path| graph_text_request(&client, &token, &base_url, &path).ok())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    });

    let recording_path = format!("/communications/onlineMeetings/{meeting_token}/recordings");
    let recording_payloads =
        collect_graph_paginated_values(&client, &token, &base_url, &recording_path)?;
    let recordings = recording_payloads
        .iter()
        .map(|payload| normalize_graph_artifact("recording", payload))
        .collect::<Vec<_>>();

    let call_record = if call_record_id.is_empty() {
        Value::Null
    } else {
        let path = format!(
            "/communications/callRecords/{}",
            percent_encode_graph_path_segment(&call_record_id)
        );
        match graph_json_request(&client, &token, "GET", &base_url, &path, None) {
            Ok(payload) => normalize_call_record_artifact(&payload),
            Err(error) => json!({
                "status": "unavailable",
                "reason": "call_record_fetch_failed",
                "error": error.to_string()
            }),
        }
    };

    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "fetch",
        "status": "live_graph_fetch_completed",
        "executed": true,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(state),
        "meetingRef": meeting_ref,
        "meeting": meeting_payload,
        "transcriptAvailable": transcript_text.is_some(),
        "transcriptArtifact": preferred_transcript,
        "transcriptPreview": transcript_text
            .as_ref()
            .map(|text| text.chars().take(240).collect::<String>()),
        "transcriptText": if bool_key(payload, &["includeTranscriptText", "include_transcript_text"]) {
            transcript_text.map(Value::from).unwrap_or(Value::Null)
        } else {
            Value::Null
        },
        "transcriptCount": transcripts.len(),
        "transcripts": transcripts,
        "recordingCount": recordings.len(),
        "recordings": recordings.into_iter().take(5).collect::<Vec<_>>(),
        "callRecord": call_record,
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "timestamp": now_iso(),
    }))
}

fn plan_pipeline_run(
    store: &AppStore,
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let job_id = string_key(payload, &["jobId", "job_id", "id"]);
    let job = state
        .as_ref()
        .and_then(|state| state.get("jobs"))
        .and_then(Value::as_object)
        .and_then(|jobs| jobs.get(&job_id))
        .cloned();
    let meeting_ref = job
        .as_ref()
        .and_then(|job| job.get("meeting_ref").or_else(|| job.get("meetingRef")))
        .cloned()
        .unwrap_or(Value::Null);
    let meeting_id = string_key(&meeting_ref, &["meeting_id", "meetingId", "id"]);
    let status = job
        .as_ref()
        .map(|job| string_key(job, &["status"]))
        .unwrap_or_default();
    let plan = json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "run",
        "status": if job.is_some() { "pipeline_runtime_plan" } else { "missing_job" },
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(state),
        "jobId": job_id,
        "found": job.is_some(),
        "job": job.as_ref().cloned().map(compact_job),
        "meetingRef": meeting_ref,
        "currentJobStatus": if status.is_empty() { Value::Null } else { json!(status) },
        "terminalStates": ["completed", "failed", "retry_scheduled"],
        "activeStates": [
            "received",
            "resolving_meeting",
            "fetching_transcript",
            "downloading_recording",
            "transcribing_audio",
            "summarizing",
            "writing_notion",
            "writing_linear",
            "sending_teams"
        ],
        "stateTransitions": [
            {"status": "resolving_meeting", "effect": "persist job before Graph meeting lookup"},
            {"status": "fetching_transcript", "effect": "list transcripts, choose preferred transcript, download transcript content"},
            {"status": "downloading_recording", "condition": "transcript missing and fallback enabled"},
            {"status": "transcribing_audio", "condition": "recording fallback selected", "runtime": "transcribe_audio"},
            {"status": "summarizing", "effect": "LLM JSON summary or heuristic fallback"},
            {"status": "writing_notion", "condition": "notion enabled and writer configured"},
            {"status": "writing_linear", "condition": "linear enabled and writer configured"},
            {"status": "sending_teams", "condition": "teams_delivery enabled and TeamsSummaryWriter configured"},
            {"status": "completed", "effect": "summary and sinks succeeded"},
            {"status": "retry_scheduled", "condition": "retryable artifact/STT availability failure"},
            {"status": "failed", "condition": "non-retryable pipeline or sink error"}
        ],
        "pipelinePhases": [
            {"phase": "load_job", "source": "TeamsPipelineStore.get_job(job_id)"},
            {"phase": "resolve_meeting_reference", "runtime": "plugins.teams_pipeline.meetings.resolve_meeting_reference"},
            {"phase": "fetch_preferred_transcript_text", "runtime": "list transcripts, select preferred transcript, download transcript content"},
            {"phase": "list_recording_artifacts", "runtime": "collect paginated meeting recordings"},
            {"phase": "enrich_meeting_with_call_record", "runtime": "optional communications/callRecords enrichment"},
            {"phase": "summarize_and_write_sinks", "runtime": "Hermes TeamsMeetingPipeline summary/delivery sink execution"},
            {"phase": "persist_result", "source": "TeamsPipelineStore.upsert_job"}
        ],
        "summaryPayloadContract": {
            "schema": "TeamsMeetingSummaryPayload",
            "fields": [
                "meeting_ref",
                "title",
                "start_time",
                "end_time",
                "participants",
                "transcript_text",
                "summary",
                "key_decisions",
                "action_items",
                "risks",
                "call_metrics",
                "source_artifacts",
                "confidence",
                "confidence_notes",
                "notion_target",
                "linear_target",
                "teams_target"
            ],
            "llmPrompt": "Hermes asks for JSON keys summary, key_decisions, action_items, risks, confidence, confidence_notes.",
            "heuristicFallback": true
        },
        "artifactStrategy": {
            "transcriptFirst": true,
            "recordingSttFallback": true,
            "selectedArtifactStrategyValues": ["transcript_first", "recording_stt_fallback"],
            "transcriptMinChars": 80,
            "ffmpegAudioExtraction": true
        },
        "sinkPlan": {
            "notion": {
                "status": "boundary",
                "sinkKey": if meeting_id.is_empty() { Value::Null } else { json!(format!("notion:{meeting_id}")) },
                "requiredEnv": "NOTION_API_KEY",
                "configured": env_present("NOTION_API_KEY"),
                "write": "NotionWriter.create/update page and TeamsPipelineStore.upsert_sink_record"
            },
            "linear": {
                "status": "boundary",
                "sinkKey": if meeting_id.is_empty() { Value::Null } else { json!(format!("linear:{meeting_id}")) },
                "requiredEnv": "LINEAR_API_KEY",
                "configured": env_present("LINEAR_API_KEY"),
                "write": "LinearWriter.create/update issue and TeamsPipelineStore.upsert_sink_record"
            },
            "teams": {
                "status": "boundary",
                "sinkKey": if meeting_id.is_empty() { Value::Null } else { json!(format!("teams:{meeting_id}")) },
                "writer": "plugins.platforms.teams.adapter.TeamsSummaryWriter when teams_delivery is enabled",
                "write": "Teams summary delivery and TeamsPipelineStore.upsert_sink_record"
            }
        },
        "errorHandling": {
            "retryableErrors": ["TeamsPipelineRetryableError", "TeamsPipelineArtifactNotFoundError", "STT empty transcript", "missing recording/transcript before availability"],
            "retryableStatus": "retry_scheduled",
            "sinkErrors": "TeamsPipelineSinkError maps to failed",
            "genericExceptionStatus": "failed",
            "errorInfoShape": {"message": "<error>", "type": "<exception>", "retryable": "<optional bool>"}
        },
        "operatorCommand": format!("hermes teams-pipeline run {job_id}"),
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "desktopExecutionBoundary": "SynthChat reports the Hermes TeamsMeetingPipeline replay plan without executing Graph, model summary, or delivery sink side effects. Execute the operator CLI through the normal terminal/process path for live replay.",
        "timestamp": now_iso(),
    });
    if graph_live_requested(payload) {
        execute_pipeline_run(store, payload, store_path, state, plan)
    } else {
        Ok(plan)
    }
}

fn execute_pipeline_run(
    store: &AppStore,
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
    plan: Value,
) -> AppResult<Value> {
    let job_id = string_key(payload, &["jobId", "job_id", "id"]);
    if job_id.is_empty() {
        return Err(AppError::BadRequest("run requires jobId".into()));
    }
    if plan["found"] != true {
        return Ok(plan);
    }
    if !bool_key(
        payload,
        &[
            "confirmPipelineRun",
            "confirm_pipeline_run",
            "confirmLiveGraphRead",
            "confirm_live_graph_read",
        ],
    ) {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "run",
            "status": "live_confirmation_required",
            "executed": false,
            "requiredFlag": "confirmPipelineRun:true",
            "reason": "Live Teams pipeline replay reads Microsoft Graph meeting artifacts and updates the local TeamsPipelineStore job record; it requires explicit confirmation.",
            "planned": plan,
            "timestamp": now_iso(),
        }));
    }

    let mut state_value = ensure_store_state(state.clone());
    let Some(original_job) = state_value
        .get("jobs")
        .and_then(Value::as_object)
        .and_then(|jobs| jobs.get(&job_id))
        .cloned()
    else {
        return Ok(plan);
    };
    let resolving_job = upsert_object_record(
        &mut state_value,
        "jobs",
        &job_id,
        json!({"status": "resolving_meeting"}),
        "job_id",
    );
    write_store_state(store_path, &state_value)?;

    let meeting_ref = original_job
        .get("meeting_ref")
        .or_else(|| original_job.get("meetingRef"))
        .cloned()
        .unwrap_or(Value::Null);
    let meeting_id = string_key(&meeting_ref, &["meeting_id", "meetingId", "id"]);
    let join_web_url = string_key(&meeting_ref, &["join_web_url", "joinWebUrl"]);
    let tenant_id = string_key(&meeting_ref, &["tenant_id", "tenantId"]);
    let call_record_id = string_key(&meeting_ref, &["metadata.call_record_id", "call_record_id"]);
    if meeting_id.is_empty() && join_web_url.is_empty() {
        let failed = persist_pipeline_failure(
            &mut state_value,
            store_path,
            &job_id,
            "Teams pipeline job missing meeting_id or join_web_url",
            true,
        )?;
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "run",
            "status": "live_pipeline_failed",
            "executed": true,
            "jobId": job_id,
            "job": compact_job(failed),
            "timestamp": now_iso(),
        }));
    }

    let fetch_payload = json!({
        "action": "fetch",
        "execute": true,
        "confirmLiveGraphRead": true,
        "meetingId": meeting_id,
        "joinWebUrl": join_web_url,
        "tenantId": tenant_id,
        "callRecordId": call_record_id,
        "includeTranscriptText": true
    });
    let fetch_result = match execute_graph_fetch(
        &fetch_payload,
        store_path,
        &Some(state_value.clone()),
        json!({}),
    ) {
        Ok(value) => value,
        Err(error) => {
            let failed = persist_pipeline_failure(
                &mut state_value,
                store_path,
                &job_id,
                &error.to_string(),
                true,
            )?;
            return Ok(json!({
                "schema": "hermes_teams_pipeline_desktop_v1",
                "action": "run",
                "status": "live_pipeline_failed",
                "executed": true,
                "jobId": job_id,
                "job": compact_job(failed),
                "error": error.to_string(),
                "timestamp": now_iso(),
            }));
        }
    };
    let transcript_text = string_key(&fetch_result, &["transcriptText"]);
    if transcript_text.trim().is_empty() {
        let retry = upsert_object_record(
            &mut state_value,
            "jobs",
            &job_id,
            json!({
                "status": "retry_scheduled",
                "error_info": {
                    "message": "No transcript text was available from Microsoft Graph fetch.",
                    "type": "TeamsPipelineArtifactNotFoundError",
                    "retryable": true
                },
                "last_fetch_result": compact_fetch_result_for_job(&fetch_result),
            }),
            "job_id",
        );
        write_store_state(store_path, &state_value)?;
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "run",
            "status": "live_pipeline_retry_scheduled",
            "executed": true,
            "jobId": job_id,
            "job": compact_job(retry),
            "fetch": compact_fetch_result_for_job(&fetch_result),
            "timestamp": now_iso(),
        }));
    }

    upsert_object_record(
        &mut state_value,
        "jobs",
        &job_id,
        json!({"status": "summarizing"}),
        "job_id",
    );
    let summary_payload = build_heuristic_summary_payload(&fetch_result, &transcript_text);
    let mut completed = upsert_object_record(
        &mut state_value,
        "jobs",
        &job_id,
        json!({
            "status": "completed",
            "summary_payload": summary_payload,
            "selected_artifact_strategy": "transcript_first",
            "last_fetch_result": compact_fetch_result_for_job(&fetch_result),
            "sink_status": {
                "notion": "boundary_not_written",
                "linear": "boundary_not_written",
                "teams": "boundary_not_written"
            }
        }),
        "job_id",
    );
    let sink_status = write_or_plan_pipeline_sinks(
        store,
        payload,
        &mut state_value,
        &job_id,
        completed
            .get("summary_payload")
            .cloned()
            .unwrap_or(Value::Null),
    )?;
    completed = upsert_object_record(
        &mut state_value,
        "jobs",
        &job_id,
        json!({"sink_status": sink_status.clone()}),
        "job_id",
    );
    write_store_state(store_path, &state_value)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "run",
        "status": "live_pipeline_completed",
        "executed": true,
        "jobId": job_id,
        "initialJob": compact_job(resolving_job),
        "job": compact_job(completed.clone()),
        "summaryPayload": completed.get("summary_payload").cloned().unwrap_or(Value::Null),
        "fetch": compact_fetch_result_for_job(&fetch_result),
        "sinkStatus": sink_status,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(&Some(state_value)),
        "timestamp": now_iso(),
    }))
}

fn plan_pipeline_summary_generation(
    store: &AppStore,
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let mut state_value = ensure_store_state(state.clone());
    let job_id = string_key(payload, &["jobId", "job_id", "id"]);
    let job = if job_id.is_empty() {
        None
    } else {
        state_value
            .get("jobs")
            .and_then(Value::as_object)
            .and_then(|jobs| jobs.get(&job_id))
            .cloned()
    };
    if !job_id.is_empty() && job.is_none() {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "summarize",
            "status": "job_not_found",
            "executed": false,
            "jobId": job_id,
            "storePath": store_path.to_string_lossy().to_string(),
            "timestamp": now_iso(),
        }));
    }

    let fetch_result = payload
        .get("fetchResult")
        .or_else(|| payload.get("fetch_result"))
        .cloned()
        .or_else(|| {
            job.as_ref()
                .and_then(|job| {
                    job.get("last_fetch_result")
                        .or_else(|| job.get("lastFetchResult"))
                })
                .cloned()
        })
        .unwrap_or_else(|| json!({}));
    let meeting_ref = payload
        .get("meetingRef")
        .or_else(|| payload.get("meeting_ref"))
        .cloned()
        .or_else(|| fetch_result.get("meetingRef").cloned())
        .or_else(|| {
            job.as_ref()
                .and_then(|job| job.get("meeting_ref").or_else(|| job.get("meetingRef")))
                .cloned()
        })
        .unwrap_or(Value::Null);
    let transcript_text = string_key(payload, &["transcriptText", "transcript_text"])
        .if_empty_then(|| string_key(&fetch_result, &["transcriptText", "transcript_text"]))
        .if_empty_then(|| {
            job.as_ref()
                .map(|job| {
                    job.get("summary_payload")
                        .or_else(|| job.get("summaryPayload"))
                        .map(|summary| string_key(summary, &["transcript_text", "transcriptText"]))
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        });
    let artifacts = summary_source_artifacts(&fetch_result);
    let prompt = build_teams_summary_prompt(&meeting_ref, &transcript_text, &artifacts);
    let system_prompt = "You summarize meeting transcripts. Return only valid JSON with keys: summary, key_decisions, action_items, risks, confidence, confidence_notes.";

    let llm_response = payload
        .get("llmResponse")
        .or_else(|| payload.get("llm_response"))
        .or_else(|| payload.get("summaryJson"))
        .or_else(|| payload.get("summary_json"));
    if llm_response.is_none() {
        let mut plan_fetch_result = fetch_result.clone();
        if !meeting_ref.is_null() && plan_fetch_result.get("meetingRef").is_none() {
            if let Some(object) = plan_fetch_result.as_object_mut() {
                object.insert("meetingRef".into(), meeting_ref.clone());
            }
        }
        let heuristic_payload =
            build_heuristic_summary_payload(&plan_fetch_result, &transcript_text);
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "summarize",
            "status": "summary_generation_plan",
            "executed": false,
            "jobId": null_if_empty(job_id),
            "llmRequest": {
                "task": "call",
                "temperature": 0.2,
                "maxTokens": 900,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": prompt}
                ]
            },
            "parserContract": teams_summary_parser_contract(),
            "heuristicFallbackPayload": compact_summary_payload(heuristic_payload),
            "storePath": store_path.to_string_lossy().to_string(),
            "timestamp": now_iso(),
        }));
    }

    let parsed = parse_summary_json_or_heuristic(llm_response.unwrap(), &transcript_text);
    let mut summary_fetch_result = fetch_result.clone();
    if !meeting_ref.is_null() && summary_fetch_result.get("meetingRef").is_none() {
        if let Some(object) = summary_fetch_result.as_object_mut() {
            object.insert("meetingRef".into(), meeting_ref.clone());
        }
    }
    let summary_payload =
        build_summary_payload_from_parsed(store, &summary_fetch_result, &transcript_text, &parsed);
    let persist = bool_key(
        payload,
        &[
            "persist",
            "confirmSummaryPersist",
            "confirm_summary_persist",
            "apply",
        ],
    );
    if persist && !job_id.is_empty() {
        let saved_job = upsert_object_record(
            &mut state_value,
            "jobs",
            &job_id,
            json!({
                "status": "completed",
                "summary_payload": summary_payload.clone(),
                "summary_source": parsed.get("source").cloned().unwrap_or_else(|| json!("llm_json")),
                "summary_error": parsed.get("error").cloned().unwrap_or(Value::Null),
            }),
            "job_id",
        );
        write_store_state(store_path, &state_value)?;
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "summarize",
            "status": "summary_persisted",
            "executed": true,
            "jobId": job_id,
            "job": compact_job(saved_job),
            "summaryPayload": compact_summary_payload(summary_payload),
            "parser": parsed.get("parser").cloned().unwrap_or(Value::Null),
            "storePath": store_path.to_string_lossy().to_string(),
            "storeStats": store_stats(&Some(state_value)),
            "timestamp": now_iso(),
        }));
    }

    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "summarize",
        "status": "summary_generated",
        "executed": false,
        "jobId": null_if_empty(job_id),
        "summaryPayload": compact_summary_payload(summary_payload),
        "parser": parsed.get("parser").cloned().unwrap_or(Value::Null),
        "persistHint": "Set persist:true or confirmSummaryPersist:true with jobId to update the TeamsPipelineStore job.",
        "timestamp": now_iso(),
    }))
}

fn plan_pipeline_sink_write(
    store: &AppStore,
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let job_id = string_key(payload, &["jobId", "job_id", "id"]);
    if job_id.is_empty() {
        return Err(AppError::BadRequest(
            "write-sinks requires jobId for a stored Teams pipeline job".into(),
        ));
    }
    let mut state_value = ensure_store_state(state.clone());
    let Some(job) = state_value
        .get("jobs")
        .and_then(Value::as_object)
        .and_then(|jobs| jobs.get(&job_id))
        .cloned()
    else {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "write-sinks",
            "status": "job_not_found",
            "executed": false,
            "jobId": job_id,
            "storePath": store_path.to_string_lossy().to_string(),
            "timestamp": now_iso(),
        }));
    };
    let summary_payload = job
        .get("summary_payload")
        .or_else(|| job.get("summaryPayload"))
        .cloned()
        .unwrap_or(Value::Null);
    if !summary_payload.is_object() {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "write-sinks",
            "status": "summary_payload_missing",
            "executed": false,
            "jobId": job_id,
            "currentJobStatus": job.get("status").cloned().unwrap_or(Value::Null),
            "reason": "Hermes sink writers require a completed TeamsMeetingSummaryPayload; run/replay the job first or upsert a summary_payload.",
            "timestamp": now_iso(),
        }));
    }

    let confirm_writes = bool_key(
        payload,
        &[
            "confirmSinkWrites",
            "confirm_sink_writes",
            "confirmDelivery",
        ],
    );
    let sink_status =
        write_or_plan_pipeline_sinks(store, payload, &mut state_value, &job_id, summary_payload)?;
    let saved_job = upsert_object_record(
        &mut state_value,
        "jobs",
        &job_id,
        json!({"sink_status": sink_status.clone()}),
        "job_id",
    );
    write_store_state(store_path, &state_value)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "write-sinks",
        "status": if confirm_writes { "sink_write_completed" } else { "sink_write_planned" },
        "executed": confirm_writes,
        "jobId": job_id,
        "job": compact_job(saved_job),
        "sinkStatus": sink_status,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(&Some(state_value)),
        "timestamp": now_iso(),
    }))
}

fn persist_pipeline_failure(
    state: &mut Value,
    store_path: &PathBuf,
    job_id: &str,
    message: &str,
    retryable: bool,
) -> AppResult<Value> {
    let saved = upsert_object_record(
        state,
        "jobs",
        job_id,
        json!({
            "status": if retryable { "retry_scheduled" } else { "failed" },
            "error_info": {
                "message": message,
                "type": "TeamsPipelineError",
                "retryable": retryable
            }
        }),
        "job_id",
    );
    write_store_state(store_path, state)?;
    Ok(saved)
}

fn compact_fetch_result_for_job(fetch_result: &Value) -> Value {
    let mut compact = fetch_result.clone();
    if let Some(object) = compact.as_object_mut() {
        if let Some(text) = object.remove("transcriptText").and_then(|value| {
            value
                .as_str()
                .map(|text| text.chars().take(240).collect::<String>())
        }) {
            object.insert("transcriptPreview".into(), json!(text));
        }
        object.remove("meeting");
        object.remove("graphRuntimeContract");
    }
    compact
}

fn build_heuristic_summary_payload(fetch_result: &Value, transcript_text: &str) -> Value {
    let parsed = heuristic_summary(transcript_text);
    build_summary_payload_from_parsed_without_store(fetch_result, transcript_text, &parsed)
}

fn build_summary_payload_from_parsed(
    store: &AppStore,
    fetch_result: &Value,
    transcript_text: &str,
    parsed: &Value,
) -> Value {
    let mut payload =
        build_summary_payload_from_parsed_without_store(fetch_result, transcript_text, parsed);
    let config = store.config().ok();
    let teams_config = config
        .as_ref()
        .map(|config| config.teams.clone())
        .unwrap_or_else(|| json!({}));
    let pipeline_config = object_key(&teams_config, &["meetingPipeline", "meeting_pipeline"])
        .unwrap_or_else(|| json!({}));
    let notion_config = object_key(&pipeline_config, &["notion"]).unwrap_or_else(|| json!({}));
    let linear_config = object_key(&pipeline_config, &["linear"]).unwrap_or_else(|| json!({}));
    let teams_delivery_config = object_key(&pipeline_config, &["teamsDelivery", "teams_delivery"])
        .unwrap_or_else(|| json!({}));
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "notion_target".into(),
            null_if_empty(string_key(&notion_config, &["database_id", "databaseId"])),
        );
        object.insert(
            "linear_target".into(),
            null_if_empty(string_key(&linear_config, &["team_id", "teamId"])),
        );
        object.insert(
            "teams_target".into(),
            null_if_empty(
                string_key(&teams_delivery_config, &["channel_id", "channelId"])
                    .if_empty_then(|| string_key(&teams_delivery_config, &["chat_id", "chatId"]))
                    .if_empty_then(|| string_key(&teams_config, &["channelId", "channel_id"]))
                    .if_empty_then(|| string_key(&teams_config, &["chatId", "chat_id"])),
            ),
        );
    }
    payload
}

fn build_summary_payload_from_parsed_without_store(
    fetch_result: &Value,
    transcript_text: &str,
    parsed: &Value,
) -> Value {
    let meeting_ref = fetch_result
        .get("meetingRef")
        .or_else(|| fetch_result.get("meeting_ref"))
        .cloned()
        .unwrap_or(Value::Null);
    let meeting_id = string_key(&meeting_ref, &["meeting_id", "meetingId", "id"]);
    let metadata = meeting_ref
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let source_artifacts = summary_source_artifacts(fetch_result);
    let call_metrics = collect_call_metrics_from_artifacts(&source_artifacts);
    json!({
        "meeting_ref": meeting_ref,
        "title": string_key(&metadata, &["subject"]).if_empty_then(|| format!("Meeting {meeting_id}")),
        "start_time": metadata.get("startDateTime").cloned().unwrap_or(Value::Null),
        "end_time": metadata.get("endDateTime").cloned().unwrap_or(Value::Null),
        "participants": collect_participants_from_meeting_metadata(&metadata),
        "transcript_text": transcript_text,
        "summary": parsed["summary"].clone(),
        "key_decisions": parsed["key_decisions"].clone(),
        "action_items": parsed["action_items"].clone(),
        "risks": parsed["risks"].clone(),
        "call_metrics": call_metrics,
        "source_artifacts": source_artifacts,
        "confidence": parsed["confidence"].clone(),
        "confidence_notes": parsed["confidence_notes"].clone(),
        "notion_target": Value::Null,
        "linear_target": Value::Null,
        "teams_target": Value::Null,
    })
}

fn summary_source_artifacts(fetch_result: &Value) -> Vec<Value> {
    let mut source_artifacts = Vec::<Value>::new();
    if let Some(artifact) = fetch_result
        .get("transcriptArtifact")
        .or_else(|| fetch_result.get("transcript_artifact"))
        .filter(|value| value.is_object())
    {
        source_artifacts.push(artifact.clone());
    }
    if let Some(call_record) = fetch_result
        .get("callRecord")
        .or_else(|| fetch_result.get("call_record"))
        .filter(|value| value.is_object())
    {
        source_artifacts.push(call_record.clone());
    }
    let recordings = fetch_result
        .get("recordings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    source_artifacts.extend(recordings);
    source_artifacts
}

fn build_teams_summary_prompt(
    meeting_ref: &Value,
    transcript_text: &str,
    artifacts: &[Value],
) -> String {
    let meeting_id = string_key(meeting_ref, &["meeting_id", "meetingId", "id"]);
    let metadata = meeting_ref
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let title = string_key(&metadata, &["subject"]).if_empty_then(|| "Unknown".into());
    let artifact_lines = artifacts
        .iter()
        .map(|artifact| {
            let artifact_type = string_key(artifact, &["artifact_type", "artifactType", "type"])
                .if_empty_then(|| "artifact".into());
            let artifact_id = string_key(artifact, &["artifact_id", "artifactId", "id"])
                .if_empty_then(|| "-".into());
            let display_name = string_key(artifact, &["display_name", "displayName", "name"]);
            format!("- {artifact_type}:{artifact_id}:{display_name}")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .if_empty_then(|| "- none".into());
    let transcript = transcript_text.chars().take(18_000).collect::<String>();
    format!(
        "Meeting ID: {meeting_id}\nTitle: {title}\nArtifacts:\n{artifact_lines}\n\nTranscript:\n{transcript}"
    )
}

fn teams_summary_parser_contract() -> Value {
    json!({
        "schema": "hermes_teams_pipeline_summary_parser_contract_desktop_v1",
        "systemPrompt": "You summarize meeting transcripts. Return only valid JSON with keys: summary, key_decisions, action_items, risks, confidence, confidence_notes.",
        "requiredKeys": ["summary", "key_decisions", "action_items", "risks", "confidence", "confidence_notes"],
        "jsonExtraction": "Trim content, then parse the substring from the first { to the last } when wrapper text or fenced blocks are present.",
        "fallback": "Use Hermes heuristic summary when the LLM response is empty or cannot be parsed.",
        "temperature": 0.2,
        "maxTokens": 900,
        "transcriptCharLimit": 18000
    })
}

fn parse_summary_json_or_heuristic(response: &Value, transcript_text: &str) -> Value {
    let raw = if response.is_object() || response.is_array() {
        response.to_string()
    } else {
        response.as_str().unwrap_or("").trim().to_string()
    };
    match parse_summary_json_response(&raw) {
        Ok(mut parsed) => {
            if let Some(object) = parsed.as_object_mut() {
                object.insert("source".into(), json!("llm_json"));
                object.insert("parser".into(), json!("json"));
            }
            parsed
        }
        Err(error) => {
            let mut parsed = heuristic_summary(transcript_text);
            if let Some(object) = parsed.as_object_mut() {
                object.insert("source".into(), json!("heuristic_fallback"));
                object.insert("parser".into(), json!("heuristic_after_parse_error"));
                object.insert("error".into(), json!(error));
            }
            parsed
        }
    }
}

fn parse_summary_json_response(content: &str) -> Result<Value, String> {
    let mut text = content.trim().to_string();
    if text.is_empty() {
        return Err("empty LLM summary response".into());
    }
    if text.starts_with("```") {
        text = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end > start {
            text = text[start..=end].to_string();
        }
    }
    let payload: Value =
        serde_json::from_str(&text).map_err(|error| format!("invalid summary JSON: {error}"))?;
    Ok(json!({
        "summary": string_key(&payload, &["summary"]),
        "key_decisions": string_array_or_empty(&payload, &["key_decisions", "keyDecisions"]),
        "action_items": string_array_or_empty(&payload, &["action_items", "actionItems"]),
        "risks": string_array_or_empty(&payload, &["risks"]),
        "confidence": string_key(&payload, &["confidence"]).if_empty_then(|| "medium".into()),
        "confidence_notes": string_key(&payload, &["confidence_notes", "confidenceNotes"]),
    }))
}

fn string_array_or_empty(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| {
            let candidate = if key.contains('.') {
                nested_value(value, key)
            } else {
                value.get(*key)
            }?;
            if let Some(items) = candidate.as_array() {
                return Some(
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| item.to_string())
                        })
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect::<Vec<_>>(),
                );
            }
            candidate.as_str().map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default()
}

fn compact_summary_payload(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        if let Some(text) = object
            .remove("transcript_text")
            .or_else(|| object.remove("transcriptText"))
            .and_then(|value| value.as_str().map(str::to_string))
        {
            object.insert(
                "transcript_preview".into(),
                json!(text.chars().take(240).collect::<String>()),
            );
        }
    }
    payload
}

fn heuristic_summary(transcript_text: &str) -> Value {
    let lines = transcript_text
        .lines()
        .map(|line| line.trim_matches(|ch: char| ch.is_whitespace() || ch == '-' || ch == '*'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let summary = lines
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1200)
        .collect::<String>()
        .if_empty_then(|| "Transcript unavailable or too sparse for a confident summary.".into());
    let action_items = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("action:")
                || lower.starts_with("todo:")
                || lower.starts_with("next step:")
                || lower.starts_with("follow up:")
        })
        .take(8)
        .cloned()
        .collect::<Vec<_>>();
    let risks = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("risk") || lower.contains("blocker")
        })
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    let key_decisions = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("decide") || lower.contains("decision")
        })
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    json!({
        "summary": summary,
        "key_decisions": key_decisions,
        "action_items": action_items,
        "risks": risks,
        "confidence": if transcript_text.trim().len() < 300 { "low" } else { "medium" },
        "confidence_notes": "Generated with heuristic fallback because no LLM summary response was available."
    })
}

fn collect_call_metrics_from_artifacts(artifacts: &[Value]) -> Value {
    let mut metrics = Map::new();
    for artifact in artifacts {
        if string_key(artifact, &["artifact_type"]) != "call_record" {
            continue;
        }
        if let Some(call_metrics) = artifact
            .get("metadata")
            .and_then(|metadata| metadata.get("metrics"))
            .and_then(Value::as_object)
        {
            for (key, value) in call_metrics {
                metrics.insert(key.clone(), value.clone());
            }
        }
    }
    metrics.insert("artifact_count".into(), json!(artifacts.len()));
    Value::Object(metrics)
}

fn collect_participants_from_meeting_metadata(metadata: &Value) -> Value {
    let participants = metadata
        .get("participants")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            let name = string_key(
                &item,
                &[
                    "displayName",
                    "identity.user.displayName",
                    "user.displayName",
                ],
            )
            .if_empty_then(|| string_key(&item, &["identity.application.displayName"]))
            .if_empty_then(|| string_key(&item, &["upn", "email"]));
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        })
        .collect::<Vec<_>>();
    json!(participants)
}

fn write_or_plan_pipeline_sinks(
    store: &AppStore,
    payload: &Value,
    state: &mut Value,
    job_id: &str,
    summary_payload: Value,
) -> AppResult<Value> {
    let meeting_id = string_key(
        summary_payload.get("meeting_ref").unwrap_or(&Value::Null),
        &["meeting_id", "meetingId", "id"],
    );
    if meeting_id.is_empty() {
        return Ok(json!({"status": "skipped", "reason": "missing_meeting_id"}));
    }
    let config = store.config().ok();
    let teams_config = config
        .as_ref()
        .map(|config| config.teams.clone())
        .unwrap_or_else(|| json!({}));
    let pipeline_config = object_key(&teams_config, &["meetingPipeline", "meeting_pipeline"])
        .unwrap_or_else(|| json!({}));
    let confirm_writes = bool_key(
        payload,
        &[
            "confirmSinkWrites",
            "confirm_sink_writes",
            "confirmDelivery",
        ],
    );
    let mut result = Map::new();

    let notion_config = object_key(&pipeline_config, &["notion"]).unwrap_or_else(|| json!({}));
    result.insert(
        "notion".into(),
        plan_or_write_single_sink(
            state,
            "notion",
            &meeting_id,
            job_id,
            bool_key(&notion_config, &["enabled"]),
            confirm_writes,
            |existing| write_notion_summary_sink(&summary_payload, &notion_config, &existing),
        )?,
    );

    let linear_config = object_key(&pipeline_config, &["linear"]).unwrap_or_else(|| json!({}));
    result.insert(
        "linear".into(),
        plan_or_write_single_sink(
            state,
            "linear",
            &meeting_id,
            job_id,
            bool_key(&linear_config, &["enabled"]),
            confirm_writes,
            |existing| {
                write_linear_summary_sink(&summary_payload, &linear_config, &existing, &meeting_id)
            },
        )?,
    );

    let teams_delivery_config = object_key(&pipeline_config, &["teamsDelivery", "teams_delivery"])
        .unwrap_or_else(|| json!({}));
    let teams_delivery_enabled =
        bool_key(&teams_delivery_config, &["enabled"]) || bool_key(&teams_config, &["enabled"]);
    result.insert(
        "teams".into(),
        plan_or_write_single_sink(
            state,
            "teams",
            &meeting_id,
            job_id,
            teams_delivery_enabled,
            confirm_writes,
            |existing| {
                write_teams_summary_sink(
                    &summary_payload,
                    &teams_delivery_config,
                    &teams_config,
                    &existing,
                    &meeting_id,
                )
            },
        )?,
    );

    Ok(Value::Object(result))
}

fn plan_or_write_single_sink<F>(
    state: &mut Value,
    sink: &str,
    meeting_id: &str,
    job_id: &str,
    enabled: bool,
    confirm_writes: bool,
    writer: F,
) -> AppResult<Value>
where
    F: FnOnce(Value) -> AppResult<Value>,
{
    let sink_key = format!("{sink}:{meeting_id}");
    if !enabled {
        let record = upsert_object_record(
            state,
            "sink_records",
            &sink_key,
            json!({
                "sink": sink,
                "meeting_id": meeting_id,
                "job_id": job_id,
                "status": "skipped",
                "reason": "sink_disabled"
            }),
            "sink_key",
        );
        return Ok(record);
    }
    if !confirm_writes {
        let record = upsert_object_record(
            state,
            "sink_records",
            &sink_key,
            json!({
                "sink": sink,
                "meeting_id": meeting_id,
                "job_id": job_id,
                "status": "planned",
                "requires_flag": "confirmSinkWrites:true"
            }),
            "sink_key",
        );
        return Ok(record);
    }
    let existing = sink_record(state, &sink_key);
    match writer(existing) {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("sink".into(), json!(sink));
                object.insert("meeting_id".into(), json!(meeting_id));
                object.insert("job_id".into(), json!(job_id));
                object.insert("status".into(), json!("written"));
            }
            Ok(upsert_object_record(
                state,
                "sink_records",
                &sink_key,
                value,
                "sink_key",
            ))
        }
        Err(error) => {
            let record = upsert_object_record(
                state,
                "sink_records",
                &sink_key,
                json!({
                    "sink": sink,
                    "meeting_id": meeting_id,
                    "job_id": job_id,
                    "status": "failed",
                    "error": error.to_string()
                }),
                "sink_key",
            );
            Ok(record)
        }
    }
}

fn write_notion_summary_sink(
    summary_payload: &Value,
    config: &Value,
    existing: &Value,
) -> AppResult<Value> {
    let api_key = non_empty_env("NOTION_API_KEY")
        .ok_or_else(|| AppError::BadRequest("NOTION_API_KEY is not configured".into()))?;
    let database_id = string_key(config, &["database_id", "databaseId"]);
    let page_id = string_key(&existing, &["page_id", "pageId"]);
    if database_id.is_empty() && page_id.is_empty() {
        return Err(AppError::BadRequest(
            "Notion sink requires database_id or an existing page_id".into(),
        ));
    }
    let body = if page_id.is_empty() {
        json!({
            "parent": {"database_id": database_id},
            "properties": notion_summary_properties(summary_payload, config),
            "children": notion_summary_blocks(summary_payload)
        })
    } else {
        json!({"properties": notion_summary_properties(summary_payload, config)})
    };
    let url = if page_id.is_empty() {
        "https://api.notion.com/v1/pages".to_string()
    } else {
        format!("https://api.notion.com/v1/pages/{page_id}")
    };
    let method = if page_id.is_empty() { "POST" } else { "PATCH" };
    let response = blocking_json_request(
        method,
        &url,
        &[
            ("Authorization", format!("Bearer {api_key}")),
            ("Notion-Version", "2025-09-03".into()),
        ],
        &body,
    )?;
    Ok(json!({
        "page_id": string_key(&response, &["id"]).if_empty_then(|| page_id),
        "url": response.get("url").cloned().unwrap_or(Value::Null),
        "raw": response
    }))
}

fn write_linear_summary_sink(
    summary_payload: &Value,
    config: &Value,
    existing: &Value,
    meeting_id: &str,
) -> AppResult<Value> {
    let api_key = non_empty_env("LINEAR_API_KEY")
        .ok_or_else(|| AppError::BadRequest("LINEAR_API_KEY is not configured".into()))?;
    let issue_id = string_key(&existing, &["issue_id", "issueId"]);
    let team_id = string_key(config, &["team_id", "teamId"]);
    let title = string_key(summary_payload, &["title"])
        .if_empty_then(|| format!("Meeting Summary: {meeting_id}"));
    let description = render_summary_markdown(summary_payload);
    let body = if issue_id.is_empty() {
        if team_id.is_empty() {
            return Err(AppError::BadRequest(
                "Linear sink requires team_id when creating a new issue".into(),
            ));
        }
        json!({
            "query": "mutation($input: IssueCreateInput!) { issueCreate(input: $input) { success issue { id identifier url } } }",
            "variables": {"input": {"teamId": team_id, "title": title, "description": description}}
        })
    } else {
        json!({
            "query": "mutation($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success issue { id identifier url } } }",
            "variables": {"id": issue_id, "input": {"title": title, "description": description}}
        })
    };
    let response = blocking_json_request(
        "POST",
        "https://api.linear.app/graphql",
        &[("Authorization", api_key)],
        &body,
    )?;
    let issue = response
        .get("data")
        .and_then(|data| data.get("issueUpdate").or_else(|| data.get("issueCreate")))
        .and_then(|payload| payload.get("issue"))
        .cloned()
        .unwrap_or(Value::Null);
    let resolved_issue_id = string_key(&issue, &["id"]);
    if resolved_issue_id.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Linear write failed: {}",
            compact_json_for_error(&response)
        )));
    }
    Ok(json!({
        "issue_id": resolved_issue_id,
        "identifier": issue.get("identifier").cloned().unwrap_or(Value::Null),
        "url": issue.get("url").cloned().unwrap_or(Value::Null),
        "raw": response
    }))
}

fn write_teams_summary_sink(
    summary_payload: &Value,
    config: &Value,
    teams_config: &Value,
    existing: &Value,
    _meeting_id: &str,
) -> AppResult<Value> {
    if !existing.is_null() && !bool_key(config, &["force_resend", "forceResend"]) {
        return Ok(existing.clone());
    }
    let mode = string_key(config, &["delivery_mode", "deliveryMode", "mode"])
        .if_empty_then(|| string_key(teams_config, &["deliveryMode", "delivery_mode"]));
    let incoming_webhook_url = string_key(config, &["incoming_webhook_url", "incomingWebhookUrl"])
        .if_empty_then(|| {
            string_key(
                teams_config,
                &["incomingWebhookUrl", "incoming_webhook_url"],
            )
        })
        .if_empty_then(|| non_empty_env("TEAMS_INCOMING_WEBHOOK_URL").unwrap_or_default());
    let markdown = render_summary_markdown(summary_payload);
    if mode == "incoming_webhook" || (!incoming_webhook_url.is_empty() && mode.is_empty()) {
        if incoming_webhook_url.is_empty() {
            return Err(AppError::BadRequest(
                "TEAMS_INCOMING_WEBHOOK_URL is required for incoming_webhook mode".into(),
            ));
        }
        let response = blocking_json_request(
            "POST",
            &incoming_webhook_url,
            &[],
            &json!({"text": markdown}),
        )?;
        return Ok(json!({
            "delivery_mode": "incoming_webhook",
            "webhook_url": incoming_webhook_url,
            "delivered": true,
            "raw": response
        }));
    }
    let access_token = string_key(config, &["access_token", "accessToken"])
        .if_empty_then(|| string_key(teams_config, &["accessToken", "access_token"]))
        .if_empty_then(|| non_empty_env("TEAMS_GRAPH_ACCESS_TOKEN").unwrap_or_default())
        .if_empty_then(|| microsoft_graph_access_token().unwrap_or_default());
    if access_token.is_empty() {
        return Err(AppError::BadRequest(
            "Teams graph delivery requires accessToken or Microsoft Graph app credentials".into(),
        ));
    }
    let chat_id = string_key(config, &["chat_id", "chatId"])
        .if_empty_then(|| string_key(teams_config, &["chatId", "chat_id"]));
    let team_id = string_key(config, &["team_id", "teamId"])
        .if_empty_then(|| string_key(teams_config, &["teamId", "team_id"]));
    let channel_id = string_key(config, &["channel_id", "channelId"])
        .if_empty_then(|| string_key(teams_config, &["channelId", "channel_id"]));
    let path = if !chat_id.is_empty() {
        format!(
            "/chats/{}/messages",
            percent_encode_graph_path_segment(&chat_id)
        )
    } else if !team_id.is_empty() && !channel_id.is_empty() {
        format!(
            "/teams/{}/channels/{}/messages",
            percent_encode_graph_path_segment(&team_id),
            percent_encode_graph_path_segment(&channel_id)
        )
    } else {
        return Err(AppError::BadRequest(
            "Teams graph delivery requires chat_id or team_id/channel_id".into(),
        ));
    };
    let base_url = non_empty_env("MSGRAPH_GRAPH_BASE_URL")
        .or_else(|| non_empty_env("MICROSOFT_GRAPH_BASE_URL"))
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into());
    let client = graph_http_client()?;
    let response = graph_json_request(
        &client,
        &access_token,
        "POST",
        &base_url,
        &path,
        Some(
            &json!({"body": {"contentType": "html", "content": render_summary_html(summary_payload)}}),
        ),
    )?;
    Ok(json!({
        "delivery_mode": "graph",
        "target_type": if chat_id.is_empty() { "channel" } else { "chat" },
        "chat_id": null_if_empty(chat_id),
        "team_id": null_if_empty(team_id),
        "channel_id": null_if_empty(channel_id),
        "message_id": response.get("id").cloned().unwrap_or(Value::Null),
        "web_url": response.get("webUrl").cloned().unwrap_or(Value::Null),
        "raw": response
    }))
}

fn blocking_json_request(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> AppResult<Value> {
    let client = graph_http_client()?;
    let method_value = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| AppError::BadRequest(format!("invalid HTTP method {method}: {error}")))?;
    let mut request = client
        .request(method_value, url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = request.json(body).send().map_err(|error| {
        AppError::BadRequest(format!("HTTP request {method} {url} failed: {error}"))
    })?;
    let status_code = response.status().as_u16();
    let ok = response.status().is_success();
    let text = response
        .text()
        .map_err(|error| AppError::BadRequest(format!("failed reading HTTP response: {error}")))?;
    let value = if text.trim().is_empty() {
        json!({"statusCode": status_code})
    } else {
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"text": text}))
    };
    if !ok {
        return Err(AppError::BadRequest(format!(
            "HTTP request {method} {url} returned {status_code}: {}",
            compact_json_for_error(&value)
        )));
    }
    Ok(value)
}

fn sink_record(state: &Value, sink_key: &str) -> Value {
    state
        .get("sink_records")
        .and_then(Value::as_object)
        .and_then(|records| records.get(sink_key))
        .cloned()
        .unwrap_or(Value::Null)
}

fn notion_summary_properties(payload: &Value, config: &Value) -> Value {
    let title_property =
        string_key(config, &["title_property", "titleProperty"]).if_empty_then(|| "Name".into());
    let summary_property = string_key(config, &["summary_property", "summaryProperty"]);
    let meeting_id_property = string_key(config, &["meeting_id_property", "meetingIdProperty"]);
    let title = string_key(payload, &["title"]).if_empty_then(|| {
        format!(
            "Meeting {}",
            string_key(
                payload.get("meeting_ref").unwrap_or(&Value::Null),
                &["meeting_id"]
            )
        )
    });
    let mut properties = Map::new();
    properties.insert(
        title_property,
        json!({"title": [{"text": {"content": truncate_chars(&title, 1900)}}]}),
    );
    if !summary_property.is_empty() {
        properties.insert(
            summary_property,
            json!({"rich_text": [{"text": {"content": truncate_chars(&string_key(payload, &["summary"]), 1900)}}]}),
        );
    }
    if !meeting_id_property.is_empty() {
        properties.insert(
            meeting_id_property,
            json!({"rich_text": [{"text": {"content": string_key(payload.get("meeting_ref").unwrap_or(&Value::Null), &["meeting_id"])}}]}),
        );
    }
    Value::Object(properties)
}

fn notion_summary_blocks(payload: &Value) -> Value {
    let sections = vec![
        ("Summary", string_key(payload, &["summary"])),
        (
            "Key Decisions",
            markdown_list_text(payload.get("key_decisions").unwrap_or(&Value::Null)),
        ),
        (
            "Action Items",
            markdown_list_text(payload.get("action_items").unwrap_or(&Value::Null)),
        ),
        (
            "Risks",
            markdown_list_text(payload.get("risks").unwrap_or(&Value::Null)),
        ),
    ];
    let mut blocks = Vec::<Value>::new();
    for (heading, body) in sections {
        blocks.push(json!({
            "object": "block",
            "type": "heading_2",
            "heading_2": {"rich_text": [{"text": {"content": heading}}]}
        }));
        blocks.push(json!({
            "object": "block",
            "type": "paragraph",
            "paragraph": {"rich_text": [{"text": {"content": truncate_chars(if body.is_empty() { "None" } else { &body }, 1900)}}]}
        }));
    }
    json!(blocks)
}

fn render_summary_markdown(payload: &Value) -> String {
    let title = string_key(payload, &["title"]).if_empty_then(|| {
        format!(
            "Meeting {}",
            string_key(
                payload.get("meeting_ref").unwrap_or(&Value::Null),
                &["meeting_id"]
            )
        )
    });
    format!(
        "# {title}\n\n## Summary\n{}\n\n## Key Decisions\n{}\n\n## Action Items\n{}\n\n## Risks\n{}\n\nConfidence: {}\n{}",
        string_key(payload, &["summary"]).if_empty_then(|| "No summary available.".into()),
        markdown_list_text(payload.get("key_decisions").unwrap_or(&Value::Null)),
        markdown_list_text(payload.get("action_items").unwrap_or(&Value::Null)),
        markdown_list_text(payload.get("risks").unwrap_or(&Value::Null)),
        string_key(payload, &["confidence"]).if_empty_then(|| "unknown".into()),
        string_key(payload, &["confidence_notes"])
    )
    .trim()
    .to_string()
}

fn render_summary_html(payload: &Value) -> String {
    let markdown = render_summary_markdown(payload);
    markdown
        .lines()
        .map(|line| {
            if let Some(title) = line.strip_prefix("# ") {
                format!("<h1>{}</h1>", html_escape(title))
            } else if let Some(title) = line.strip_prefix("## ") {
                format!("<h2>{}</h2>", html_escape(title))
            } else if let Some(item) = line.strip_prefix("- ") {
                format!("<p>&bull; {}</p>", html_escape(item))
            } else if line.trim().is_empty() {
                String::new()
            } else {
                format!("<p>{}</p>", html_escape(line))
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn markdown_list_text(value: &Value) -> String {
    let items = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if items.is_empty() {
        "- None".into()
    } else {
        items.join("\n")
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn plan_graph_subscribe(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let resource = string_key(payload, &["resource"]);
    let notification_url = string_key(payload, &["notificationUrl", "notification_url"]);
    if resource.is_empty() || notification_url.is_empty() {
        return Err(AppError::BadRequest(
            "subscribe requires resource and notificationUrl".into(),
        ));
    }
    let change_type = string_key(payload, &["changeType", "change_type"])
        .if_empty_then(|| default_change_type_for_resource(&resource));
    let expiration = string_key(
        payload,
        &["expirationDateTime", "expiration_datetime", "expiration"],
    )
    .if_empty_then(|| "hermes_cli_default_now_plus_1h".into());
    let tls_version = string_key(
        payload,
        &[
            "latestSupportedTlsVersion",
            "latest_supported_tls_version",
            "tlsVersion",
        ],
    )
    .if_empty_then(|| "v1_2".into());
    let mut body = Map::new();
    body.insert("changeType".into(), json!(change_type));
    body.insert("notificationUrl".into(), json!(notification_url));
    body.insert("resource".into(), json!(resource));
    body.insert("expirationDateTime".into(), json!(expiration));
    body.insert("latestSupportedTlsVersion".into(), json!(tls_version));
    let client_state = string_key(payload, &["clientState", "client_state"]);
    if !client_state.is_empty() {
        body.insert("clientState".into(), json!(client_state));
    }
    let lifecycle_url = string_key(
        payload,
        &["lifecycleNotificationUrl", "lifecycle_notification_url"],
    );
    if !lifecycle_url.is_empty() {
        body.insert("lifecycleNotificationUrl".into(), json!(lifecycle_url));
    }
    let body = Value::Object(body);
    if graph_live_requested(payload) {
        return execute_graph_subscription_request(
            payload,
            store_path,
            state,
            "subscribe",
            "POST",
            "/subscriptions",
            body,
        );
    }
    Ok(graph_subscription_plan(
        store_path,
        state,
        "subscribe",
        "POST",
        "/subscriptions",
        body,
    ))
}

fn plan_graph_renew_subscription(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let subscription_id = string_key(payload, &["subscriptionId", "subscription_id", "id"]);
    let expiration = string_key(
        payload,
        &["expirationDateTime", "expiration_datetime", "expiration"],
    );
    if subscription_id.is_empty() || expiration.is_empty() {
        return Err(AppError::BadRequest(
            "renew-subscription requires subscriptionId and expiration".into(),
        ));
    }
    let path = format!("/subscriptions/{subscription_id}");
    let body = json!({"expirationDateTime": expiration});
    if graph_live_requested(payload) {
        return execute_graph_subscription_request(
            payload,
            store_path,
            state,
            "renew-subscription",
            "PATCH",
            &path,
            body,
        );
    }
    Ok(graph_subscription_plan(
        store_path,
        state,
        "renew-subscription",
        "PATCH",
        &path,
        body,
    ))
}

fn plan_graph_delete_subscription(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let subscription_id = string_key(payload, &["subscriptionId", "subscription_id", "id"]);
    if subscription_id.is_empty() {
        return Err(AppError::BadRequest(
            "delete-subscription requires subscriptionId".into(),
        ));
    }
    let path = format!("/subscriptions/{subscription_id}");
    if graph_live_requested(payload) {
        return execute_graph_subscription_request(
            payload,
            store_path,
            state,
            "delete-subscription",
            "DELETE",
            &path,
            Value::Null,
        );
    }
    Ok(graph_subscription_plan(
        store_path,
        state,
        "delete-subscription",
        "DELETE",
        &path,
        Value::Null,
    ))
}

fn graph_subscription_plan(
    store_path: &PathBuf,
    state: &Option<Value>,
    action: &str,
    method: &str,
    path: &str,
    body: Value,
) -> Value {
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": action,
        "status": "graph_request_planned",
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(state),
        "graphRequest": {
            "method": method,
            "path": path,
            "baseUrl": "https://graph.microsoft.com/v1.0",
            "body": body
        },
        "operatorCommand": format!("hermes teams-pipeline {action}"),
        "requiredEnv": ["MSGRAPH_TENANT_ID", "MSGRAPH_CLIENT_ID", "MSGRAPH_CLIENT_SECRET"],
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "desktopExecutionBoundary": "This SynthChat action returns the Hermes Microsoft Graph request shape without sending it. Execute the operator CLI through the normal terminal/process path for live Graph mutation.",
        "timestamp": now_iso(),
    })
}

fn graph_live_requested(payload: &Value) -> bool {
    bool_key(payload, &["execute", "live", "apply"])
}

fn execute_graph_subscription_request(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
    action: &str,
    method: &str,
    path: &str,
    body: Value,
) -> AppResult<Value> {
    let plan = graph_subscription_plan(store_path, state, action, method, path, body.clone());
    if !bool_key(
        payload,
        &["confirmLiveGraphMutation", "confirm_live_graph_mutation"],
    ) {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": action,
            "status": "live_confirmation_required",
            "executed": false,
            "requiredFlag": "confirmLiveGraphMutation:true",
            "reason": "Live Microsoft Graph subscription mutations are external write operations and require explicit confirmation in addition to tool approval.",
            "planned": plan,
            "timestamp": now_iso(),
        }));
    }

    let token = microsoft_graph_access_token()?;
    let base_url = non_empty_env("MSGRAPH_GRAPH_BASE_URL")
        .or_else(|| non_empty_env("MICROSOFT_GRAPH_BASE_URL"))
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into());
    let client = graph_http_client()?;
    let (status_code, response_json) =
        graph_json_request_with_status(&client, &token, method, &base_url, path, Some(&body))
            .map_err(|error| {
                AppError::BadRequest(format!("Microsoft Graph {action} failed: {error}"))
            })?;
    let (store_updated, saved_record) =
        sync_graph_subscription_result(action, path, &body, &response_json, store_path, state)?;
    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": action,
        "status": "live_graph_mutation_completed",
        "executed": true,
        "httpStatus": status_code,
        "graphRequest": {
            "method": method,
            "path": path,
            "baseUrl": base_url,
            "body": body
        },
        "graphResponse": response_json,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeUpdated": store_updated,
        "subscription": saved_record,
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "timestamp": now_iso(),
    }))
}

fn graph_http_client() -> AppResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(StdDuration::from_secs(30))
        .user_agent("SynthChat-teams-pipeline/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Graph client: {error}")))
}

fn graph_json_request(
    client: &reqwest::blocking::Client,
    token: &str,
    method: &str,
    base_url: &str,
    path_or_url: &str,
    body: Option<&Value>,
) -> AppResult<Value> {
    graph_json_request_with_status(client, token, method, base_url, path_or_url, body)
        .map(|(_, value)| value)
}

fn graph_json_request_with_status(
    client: &reqwest::blocking::Client,
    token: &str,
    method: &str,
    base_url: &str,
    path_or_url: &str,
    body: Option<&Value>,
) -> AppResult<(u16, Value)> {
    let url = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        path_or_url.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            path_or_url.trim_start_matches('/')
        )
    };
    let method_value = reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| {
        AppError::BadRequest(format!("invalid Microsoft Graph method {method}: {error}"))
    })?;
    let mut request = client
        .request(method_value, &url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body.filter(|value| !value.is_null()) {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
    }
    let response = request.send().map_err(|error| {
        AppError::BadRequest(format!(
            "Microsoft Graph request {method} {url} failed: {error}"
        ))
    })?;
    let status_code = response.status().as_u16();
    let ok = response.status().is_success();
    let text = response.text().map_err(|error| {
        AppError::BadRequest(format!(
            "failed reading Microsoft Graph response for {method} {url}: {error}"
        ))
    })?;
    let response_json = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"text": text}))
    };
    if !ok {
        return Err(AppError::BadRequest(format!(
            "Microsoft Graph request {method} {url} returned HTTP {status_code}: {}",
            compact_json_for_error(&response_json)
        )));
    }
    Ok((status_code, response_json))
}

fn graph_text_request(
    client: &reqwest::blocking::Client,
    token: &str,
    base_url: &str,
    path_or_url: &str,
) -> AppResult<String> {
    let url = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        path_or_url.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            path_or_url.trim_start_matches('/')
        )
    };
    let response = client
        .get(&url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "text/plain, text/vtt, */*")
        .send()
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Microsoft Graph text request {url} failed: {error}"
            ))
        })?;
    let status_code = response.status().as_u16();
    if !response.status().is_success() {
        let text = response.text().unwrap_or_default();
        return Err(AppError::BadRequest(format!(
            "Microsoft Graph text request {url} returned HTTP {status_code}: {}",
            text.chars().take(600).collect::<String>()
        )));
    }
    response.text().map_err(|error| {
        AppError::BadRequest(format!(
            "failed reading Microsoft Graph text response for {url}: {error}"
        ))
    })
}

fn collect_graph_paginated_values(
    client: &reqwest::blocking::Client,
    token: &str,
    base_url: &str,
    path: &str,
) -> AppResult<Vec<Value>> {
    let mut next = Some(path.to_string());
    let mut values = Vec::<Value>::new();
    let mut pages = 0usize;
    while let Some(path_or_url) = next.take() {
        pages += 1;
        if pages > 100 {
            return Err(AppError::BadRequest(
                "Microsoft Graph pagination exceeded 100 pages".into(),
            ));
        }
        let page = graph_json_request(client, token, "GET", base_url, &path_or_url, None)?;
        if let Some(items) = page.get("value").and_then(Value::as_array) {
            values.extend(items.iter().cloned());
        } else if page.is_object() {
            values.push(page.clone());
        }
        next = page
            .get("@odata.nextLink")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    Ok(values)
}

fn microsoft_graph_access_token() -> AppResult<String> {
    let tenant_id = non_empty_env("MSGRAPH_TENANT_ID")
        .ok_or_else(|| AppError::BadRequest("MSGRAPH_TENANT_ID is required".into()))?;
    let client_id = non_empty_env("MSGRAPH_CLIENT_ID")
        .ok_or_else(|| AppError::BadRequest("MSGRAPH_CLIENT_ID is required".into()))?;
    let client_secret = non_empty_env("MSGRAPH_CLIENT_SECRET")
        .ok_or_else(|| AppError::BadRequest("MSGRAPH_CLIENT_SECRET is required".into()))?;
    let token_url = non_empty_env("MSGRAPH_TOKEN_URL").unwrap_or_else(|| {
        format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token")
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(StdDuration::from_secs(30))
        .user_agent("SynthChat-teams-pipeline/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Graph token client: {error}"))
        })?;
    let response = client
        .post(&token_url)
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("scope", "https://graph.microsoft.com/.default"),
            ("grant_type", "client_credentials"),
        ])
        .send()
        .map_err(|error| {
            AppError::BadRequest(format!("Microsoft Graph token request failed: {error}"))
        })?;
    let status = response.status();
    let text = response.text().map_err(|error| {
        AppError::BadRequest(format!(
            "failed reading Microsoft Graph token response: {error}"
        ))
    })?;
    let value = serde_json::from_str::<Value>(&text).map_err(|error| {
        AppError::BadRequest(format!("invalid Microsoft Graph token JSON: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Microsoft Graph token request returned HTTP {}: {}",
            status.as_u16(),
            compact_json_for_error(&value)
        )));
    }
    value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("Microsoft Graph token response missing access_token".into())
        })
}

fn sync_graph_subscription_result(
    action: &str,
    path: &str,
    request_body: &Value,
    response: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<(bool, Value)> {
    let mut state = ensure_store_state(state.clone());
    if action == "delete-subscription" {
        let subscription_id = path
            .trim_matches('/')
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string();
        let removed = state
            .get_mut("subscriptions")
            .and_then(Value::as_object_mut)
            .and_then(|subscriptions| subscriptions.remove(&subscription_id))
            .is_some();
        if removed {
            write_store_state(store_path, &state)?;
        }
        return Ok((
            removed,
            json!({"subscription_id": subscription_id, "removed": removed}),
        ));
    }

    let mut record = response
        .as_object()
        .map(|object| Value::Object(object.clone()))
        .unwrap_or_else(|| json!({}));
    if let Some(object) = record.as_object_mut() {
        for key in [
            "changeType",
            "notificationUrl",
            "resource",
            "expirationDateTime",
            "clientState",
        ] {
            if !object.contains_key(key) {
                if let Some(value) = request_body.get(key) {
                    object.insert(key.into(), value.clone());
                }
            }
        }
    }
    let subscription_id = string_key(&record, &["id", "subscription_id", "subscriptionId"])
        .if_empty_then(|| {
            path.trim_matches('/')
                .split('/')
                .next_back()
                .unwrap_or("")
                .to_string()
        });
    if subscription_id.is_empty() {
        return Ok((false, Value::Null));
    }
    normalize_subscription_record(&mut record, &subscription_id);
    let saved = upsert_object_record(
        &mut state,
        "subscriptions",
        &subscription_id,
        record,
        "subscription_id",
    );
    write_store_state(store_path, &state)?;
    Ok((true, saved))
}

fn compact_json_for_error(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    text.chars().take(600).collect()
}

fn plan_graph_subscription_maintenance(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
) -> AppResult<Value> {
    let renew_within_hours =
        int_key(payload, &["renewWithinHours", "renew_within_hours"], 24).clamp(1, 24 * 30);
    let extend_hours = int_key(payload, &["extendHours", "extend_hours"], 24).clamp(1, 24 * 30);
    let client_state = string_key(payload, &["clientState", "client_state"])
        .if_empty_then(|| non_empty_env("MSGRAPH_WEBHOOK_CLIENT_STATE").unwrap_or_default());
    if graph_live_requested(payload) {
        return execute_graph_subscription_maintenance(
            payload,
            store_path,
            state,
            renew_within_hours,
            extend_hours,
            &client_state,
        );
    }
    Ok(local_graph_subscription_maintenance_plan(
        store_path,
        state,
        renew_within_hours,
        extend_hours,
        &client_state,
    ))
}

fn local_graph_subscription_maintenance_plan(
    store_path: &PathBuf,
    state: &Option<Value>,
    renew_within_hours: i64,
    extend_hours: i64,
    client_state: &str,
) -> Value {
    let now = Utc::now();
    let threshold_seconds = renew_within_hours * 3600;
    let mut candidates = Vec::<Value>::new();
    let mut skipped = Vec::<Value>::new();
    for subscription in object_entries(state, "subscriptions") {
        let subscription_id = string_key(
            &subscription,
            &["subscription_id", "subscriptionId", "id", "subscriptionID"],
        );
        if subscription_id.is_empty() {
            skipped.push(json!({"reason": "missing_subscription_id"}));
            continue;
        }
        let status = string_key(&subscription, &["status"]);
        if !status.is_empty() && !status.eq_ignore_ascii_case("active") {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "inactive_local_status",
                "status": status
            }));
            continue;
        }
        let subscription_client_state = string_key(&subscription, &["client_state", "clientState"]);
        if !client_state.is_empty() && subscription_client_state != client_state {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "client_state_mismatch"
            }));
            continue;
        }
        let current_expiration = string_key(
            &subscription,
            &["expiration_datetime", "expirationDateTime"],
        );
        let Some(expiration) = parse_graph_datetime(&current_expiration) else {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "missing_or_invalid_expiration",
                "expiration_datetime": current_expiration
            }));
            continue;
        };
        let seconds_until_expiry = (expiration - now).num_seconds();
        if seconds_until_expiry < 0 {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "already_expired",
                "expiration_datetime": format_graph_datetime(expiration),
                "wouldMarkLocalStatus": "expired"
            }));
            continue;
        }
        if seconds_until_expiry > threshold_seconds {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_due",
                "expires_in_seconds": seconds_until_expiry
            }));
            continue;
        }
        let new_expiration = expiration.max(now) + Duration::hours(extend_hours);
        let new_expiration = format_graph_datetime(new_expiration);
        candidates.push(json!({
            "subscription_id": subscription_id,
            "resource": string_key(&subscription, &["resource"]),
            "current_expiration": format_graph_datetime(expiration),
            "new_expiration": new_expiration,
            "expires_in_seconds": seconds_until_expiry,
            "plannedRequest": {
                "method": "PATCH",
                "path": format!("/subscriptions/{subscription_id}"),
                "body": {"expirationDateTime": new_expiration}
            }
        }));
    }
    json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "maintain-subscriptions",
        "status": "maintenance_plan",
        "dryRun": true,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(state),
        "thresholdHours": renew_within_hours,
        "extendHours": extend_hours,
        "candidateCount": candidates.len(),
        "candidates": candidates,
        "skipped": skipped,
        "plannedRemoteSync": {
            "method": "GET",
            "path": "/subscriptions",
            "syncManagedRecords": true,
            "markMissingRemoteLocally": true
        },
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, state),
        "desktopExecutionBoundary": "Hermes maintain-subscriptions compares live GET /subscriptions results with TeamsPipelineStore and patches near-expiry managed subscriptions. SynthChat reports the same maintenance contract and local-store candidates without issuing Graph requests.",
        "timestamp": now_iso(),
    })
}

fn execute_graph_subscription_maintenance(
    payload: &Value,
    store_path: &PathBuf,
    state: &Option<Value>,
    renew_within_hours: i64,
    extend_hours: i64,
    client_state: &str,
) -> AppResult<Value> {
    let plan = local_graph_subscription_maintenance_plan(
        store_path,
        state,
        renew_within_hours,
        extend_hours,
        client_state,
    );
    if !bool_key(
        payload,
        &["confirmLiveGraphMutation", "confirm_live_graph_mutation"],
    ) {
        return Ok(json!({
            "schema": "hermes_teams_pipeline_desktop_v1",
            "action": "maintain-subscriptions",
            "status": "live_confirmation_required",
            "executed": false,
            "requiredFlag": "confirmLiveGraphMutation:true",
            "reason": "Live Microsoft Graph subscription maintenance reads remote subscriptions and can PATCH/delete local subscription state; it requires explicit confirmation in addition to tool approval.",
            "planned": plan,
            "timestamp": now_iso(),
        }));
    }

    let dry_run = bool_key(payload, &["dryRun", "dry_run"]);
    let token = microsoft_graph_access_token()?;
    let base_url = non_empty_env("MSGRAPH_GRAPH_BASE_URL")
        .or_else(|| non_empty_env("MICROSOFT_GRAPH_BASE_URL"))
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into());
    let client = graph_http_client()?;
    let remote_subscriptions =
        collect_graph_paginated_values(&client, &token, &base_url, "/subscriptions")?;
    let mut state = ensure_store_state(state.clone());
    let local_ids = store_section_keys(&state, "subscriptions");
    let now = Utc::now();
    let threshold_seconds = renew_within_hours * 3600;
    let mut remote_ids = HashSet::<String>::new();
    let mut synced = 0usize;
    let mut candidates = Vec::<Value>::new();
    let mut renewed = Vec::<Value>::new();
    let mut skipped = Vec::<Value>::new();

    for raw in remote_subscriptions
        .iter()
        .filter(|value| value.is_object())
    {
        let subscription_id = string_key(raw, &["id", "subscription_id", "subscriptionId"]);
        if subscription_id.is_empty() {
            continue;
        }
        let managed = local_ids.contains(&subscription_id)
            || (!client_state.is_empty()
                && string_key(raw, &["clientState", "client_state"]) == client_state);
        if !managed {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_managed_by_teams_pipeline"
            }));
            continue;
        }

        remote_ids.insert(subscription_id.clone());
        let mut normalized = raw.clone();
        normalize_subscription_record(&mut normalized, &subscription_id);
        let saved = upsert_object_record(
            &mut state,
            "subscriptions",
            &subscription_id,
            normalized,
            "subscription_id",
        );
        synced += 1;

        let expiration_raw = string_key(raw, &["expirationDateTime", "expiration_datetime"])
            .if_empty_then(|| string_key(&saved, &["expiration_datetime", "expirationDateTime"]));
        let Some(expiration) = parse_graph_datetime(&expiration_raw) else {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "missing_expiration"
            }));
            continue;
        };
        let seconds_until_expiry = (expiration - now).num_seconds();
        if seconds_until_expiry < 0 {
            upsert_object_record(
                &mut state,
                "subscriptions",
                &subscription_id,
                json!({
                    "status": "expired",
                    "expiration_datetime": format_graph_datetime(expiration),
                }),
                "subscription_id",
            );
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "already_expired",
                "expiration_datetime": format_graph_datetime(expiration),
            }));
            continue;
        }
        if seconds_until_expiry > threshold_seconds {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_due",
                "expires_in_seconds": seconds_until_expiry,
            }));
            continue;
        }

        let new_expiration =
            format_graph_datetime(expiration.max(now) + Duration::hours(extend_hours));
        let candidate = json!({
            "subscription_id": subscription_id,
            "resource": string_key(raw, &["resource"]),
            "current_expiration": format_graph_datetime(expiration),
            "new_expiration": new_expiration,
            "plannedRequest": {
                "method": "PATCH",
                "path": format!("/subscriptions/{subscription_id}"),
                "body": {"expirationDateTime": new_expiration}
            }
        });
        candidates.push(candidate.clone());
        if dry_run {
            continue;
        }

        let path = format!("/subscriptions/{subscription_id}");
        let body = json!({"expirationDateTime": new_expiration});
        let patched = graph_json_request(&client, &token, "PATCH", &base_url, &path, Some(&body))?;
        let mut merged = raw.as_object().cloned().unwrap_or_default();
        if let Some(patched_object) = patched.as_object() {
            for (key, value) in patched_object {
                merged.insert(key.clone(), value.clone());
            }
        }
        merged.insert("id".into(), json!(subscription_id));
        merged.insert(
            "expirationDateTime".into(),
            body["expirationDateTime"].clone(),
        );
        merged.insert("status".into(), json!("active"));
        merged.insert("latestRenewalAt".into(), json!(now_iso()));
        let mut record = Value::Object(merged);
        normalize_subscription_record(&mut record, &subscription_id);
        let saved = upsert_object_record(
            &mut state,
            "subscriptions",
            &subscription_id,
            record,
            "subscription_id",
        );
        renewed.push(json!({
            "subscription_id": candidate["subscription_id"],
            "resource": candidate["resource"],
            "current_expiration": candidate["current_expiration"],
            "new_expiration": candidate["new_expiration"],
            "result": patched,
            "subscription": saved
        }));
    }

    let mut missing_remote = Vec::<Value>::new();
    for subscription_id in store_section_keys(&state, "subscriptions") {
        if remote_ids.contains(&subscription_id) {
            continue;
        }
        let saved = upsert_object_record(
            &mut state,
            "subscriptions",
            &subscription_id,
            json!({
                "status": "missing_remote",
                "last_seen_missing_remote_at": now_iso(),
            }),
            "subscription_id",
        );
        missing_remote.push(json!({
            "subscription_id": subscription_id,
            "status": "missing_remote",
            "subscription": saved
        }));
    }
    write_store_state(store_path, &state)?;
    let refreshed_state = Some(state.clone());

    Ok(json!({
        "schema": "hermes_teams_pipeline_desktop_v1",
        "action": "maintain-subscriptions",
        "status": "live_graph_maintenance_completed",
        "success": true,
        "executed": !dry_run,
        "dryRun": dry_run,
        "storePath": store_path.to_string_lossy().to_string(),
        "storeStats": store_stats(&refreshed_state),
        "graphRequest": {
            "method": "GET",
            "path": "/subscriptions",
            "baseUrl": base_url
        },
        "remoteSubscriptionCount": remote_subscriptions.len(),
        "syncedSubscriptionCount": synced,
        "candidateCount": candidates.len(),
        "renewedCount": renewed.len(),
        "missingRemoteCount": missing_remote.len(),
        "thresholdHours": renew_within_hours,
        "extendHours": extend_hours,
        "candidates": candidates,
        "renewed": renewed,
        "skipped": skipped,
        "missingRemote": missing_remote,
        "graphRuntimeContract": teams_graph_runtime_contract(store_path, &refreshed_state),
        "timestamp": now_iso(),
    }))
}

fn parse_graph_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn format_graph_datetime(value: DateTime<Utc>) -> String {
    value
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .replace("+00:00", "Z")
}

fn percent_encode_graph_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if allowed {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if allowed {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn graph_meeting_ref_from_payload(payload: &Value, tenant_id: &str) -> Value {
    let organizer_user_id = string_key(payload, &["organizer.identity.user.id"]);
    let thread_id = string_key(payload, &["chatInfo.threadId", "threadId"]);
    let mut metadata = Map::new();
    for key in [
        "subject",
        "startDateTime",
        "endDateTime",
        "createdDateTime",
        "participants",
    ] {
        if let Some(value) = payload.get(key).filter(|value| !value.is_null()) {
            metadata.insert(key.into(), value.clone());
        }
    }
    json!({
        "meeting_id": string_key(payload, &["id"]),
        "organizer_user_id": if organizer_user_id.is_empty() { Value::Null } else { json!(organizer_user_id) },
        "join_web_url": null_if_empty(string_key(payload, &["joinWebUrl", "join_web_url"])),
        "calendar_event_id": null_if_empty(string_key(payload, &["calendarEventId", "calendar_event_id"])),
        "thread_id": null_if_empty(thread_id),
        "tenant_id": null_if_empty(tenant_id.to_string().if_empty_then(|| string_key(payload, &["tenantId", "tenant_id"]))),
        "metadata": Value::Object(metadata),
    })
}

fn normalize_graph_artifact(artifact_type: &str, payload: &Value) -> Value {
    let artifact_id = string_key(payload, &["id"]);
    let download_url = payload
        .get("@microsoft.graph.downloadUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
        .if_empty_then(|| {
            string_key(
                payload,
                &["downloadUrl", "recordingContentUrl", "transcriptContentUrl"],
            )
        });
    let source_url = string_key(payload, &["webUrl", "contentUrl"]);
    json!({
        "artifact_type": artifact_type,
        "artifact_id": artifact_id,
        "display_name": null_if_empty(string_key(payload, &["displayName", "name"])),
        "content_type": null_if_empty(string_key(payload, &["contentType", "fileMimeType"])),
        "source_url": null_if_empty(source_url),
        "download_url": null_if_empty(download_url),
        "created_at": null_if_empty(string_key(payload, &["createdDateTime"])),
        "available_at": null_if_empty(string_key(payload, &["lastModifiedDateTime", "meetingEndDateTime"])),
        "size_bytes": payload.get("size").cloned().unwrap_or(Value::Null),
        "metadata": payload.clone(),
    })
}

fn select_preferred_graph_transcript(transcripts: &[Value]) -> Option<Value> {
    let mut candidates = transcripts
        .iter()
        .filter(|artifact| string_key(artifact, &["artifact_type"]) == "transcript")
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        graph_transcript_sort_key(right).cmp(&graph_transcript_sort_key(left))
    });
    candidates.into_iter().next()
}

fn graph_transcript_sort_key(artifact: &Value) -> (i32, i32, String) {
    let status = string_key(artifact, &["metadata.status"]).to_ascii_lowercase();
    let is_completed = matches!(status.as_str(), "available" | "completed" | "succeeded") as i32;
    let has_download = (!string_key(artifact, &["download_url"])
        .if_empty_then(|| string_key(artifact, &["source_url"]))
        .is_empty()) as i32;
    let timestamp = string_key(artifact, &["available_at"])
        .if_empty_then(|| string_key(artifact, &["created_at"]));
    (is_completed, has_download, timestamp)
}

fn graph_artifact_download_path(
    meeting_token: &str,
    artifact: &Value,
    collection: &str,
) -> Option<String> {
    let download_url = string_key(artifact, &["download_url"]);
    if !download_url.is_empty() {
        return Some(download_url);
    }
    let artifact_id = string_key(artifact, &["artifact_id"]);
    if artifact_id.is_empty() {
        return None;
    }
    Some(format!(
        "/communications/onlineMeetings/{meeting_token}/{collection}/{}/content",
        percent_encode_graph_path_segment(&artifact_id)
    ))
}

fn normalize_call_record_artifact(payload: &Value) -> Value {
    let call_record_id = string_key(payload, &["id"]);
    if call_record_id.is_empty() {
        return Value::Null;
    }
    let participant_count = payload
        .get("participants")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let session_count = payload
        .get("sessions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "artifact_type": "call_record",
        "artifact_id": call_record_id,
        "display_name": null_if_empty(string_key(payload, &["type"]).if_empty_then(|| "call_record".into())),
        "source_url": null_if_empty(string_key(payload, &["webUrl"])),
        "created_at": null_if_empty(string_key(payload, &["startDateTime"])),
        "available_at": null_if_empty(string_key(payload, &["endDateTime"])),
        "metadata": {
            "call_record": payload.clone(),
            "metrics": {
                "version": payload.get("version").cloned().unwrap_or(Value::Null),
                "modalities": payload.get("modalities").cloned().unwrap_or(Value::Null),
                "participant_count": participant_count,
                "session_count": session_count,
                "organizer": null_if_empty(string_key(payload, &["organizer.identity.user.id"]))
            }
        }
    })
}

fn default_change_type_for_resource(resource: &str) -> String {
    let normalized = resource.trim().to_ascii_lowercase();
    if normalized.starts_with("communications/onlinemeetings/getalltranscripts")
        || normalized.starts_with("communications/onlinemeetings/getallrecordings")
        || normalized.starts_with("communications/callrecords")
    {
        "created".into()
    } else {
        "updated".into()
    }
}

fn teams_pipeline_store_path(store: &AppStore, payload: &Value) -> PathBuf {
    let explicit = string_key(payload, &["storePath", "store_path"]);
    if !explicit.is_empty() {
        return PathBuf::from(explicit);
    }
    if let Some(path) = non_empty_env("MSGRAPH_WEBHOOK_STORE_PATH") {
        return PathBuf::from(path);
    }
    let base = env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| store.data_dir().join(".hermes"));
    base.join(DEFAULT_STORE_FILENAME)
}

fn read_store_state(path: &PathBuf) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

fn ensure_store_state(state: Option<Value>) -> Value {
    let mut object = state
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    for key in [
        "subscriptions",
        "notification_receipts",
        "event_timestamps",
        "jobs",
        "sink_records",
    ] {
        if !object.get(key).map(Value::is_object).unwrap_or(false) {
            object.insert(key.into(), json!({}));
        }
    }
    Value::Object(object)
}

fn write_store_state(path: &PathBuf, value: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn upsert_object_record(
    state: &mut Value,
    section: &str,
    key: &str,
    record: Value,
    id_field: &str,
) -> Value {
    let now = now_iso();
    let section_object = state
        .get_mut(section)
        .and_then(Value::as_object_mut)
        .expect("store state section is initialized");
    let existing = section_object
        .get(key)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let created_at = existing
        .get("created_at")
        .or_else(|| existing.get("createdAt"))
        .cloned()
        .unwrap_or_else(|| json!(now.clone()));
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    if let Some(record_object) = record.as_object() {
        for (field, value) in record_object {
            merged.insert(field.clone(), value.clone());
        }
    }
    merged.insert(id_field.into(), json!(key));
    merged.entry("created_at").or_insert(created_at);
    merged.insert("updated_at".into(), json!(now));
    let saved = Value::Object(merged);
    section_object.insert(key.into(), saved.clone());
    saved
}

fn normalize_subscription_record(record: &mut Value, subscription_id: &str) {
    let Some(object) = record.as_object_mut() else {
        return;
    };
    if let Some(value) = object
        .remove("changeType")
        .or_else(|| object.remove("change_type"))
    {
        object.insert("change_type".into(), value);
    }
    if let Some(value) = object
        .remove("notificationUrl")
        .or_else(|| object.remove("notification_url"))
    {
        object.insert("notification_url".into(), value);
    }
    if let Some(value) = object
        .remove("expirationDateTime")
        .or_else(|| object.remove("expiration_datetime"))
    {
        object.insert("expiration_datetime".into(), value);
    }
    if let Some(value) = object
        .remove("clientState")
        .or_else(|| object.remove("client_state"))
    {
        object.insert("client_state".into(), value);
    }
    if let Some(value) = object
        .remove("latestRenewalAt")
        .or_else(|| object.remove("latest_renewal_at"))
    {
        object.insert("latest_renewal_at".into(), value);
    }
    object.insert("subscription_id".into(), json!(subscription_id));
}

fn webhook_notifications(payload: &Value) -> Option<Vec<Value>> {
    if let Some(items) = payload.get("value").and_then(Value::as_array) {
        return Some(items.clone());
    }
    if let Some(body_items) = payload
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
    {
        return Some(body_items.clone());
    }
    if let Some(notification) = payload
        .get("notification")
        .filter(|value| value.is_object())
    {
        return Some(vec![notification.clone()]);
    }
    if payload
        .get("resource")
        .or_else(|| payload.get("resourceData"))
        .is_some()
    {
        return Some(vec![payload.clone()]);
    }
    None
}

fn webhook_client_state_allowed(notification: &Value, payload: &Value) -> bool {
    let expected = string_key(
        payload,
        &[
            "clientState",
            "client_state",
            "expectedClientState",
            "expected_client_state",
            "msgraphClientState",
            "msgraph_client_state",
        ],
    )
    .if_empty_then(|| non_empty_env("MSGRAPH_WEBHOOK_CLIENT_STATE").unwrap_or_default());
    if expected.is_empty() {
        return false;
    }
    let provided = string_key(notification, &["clientState", "client_state"]);
    constant_time_eq(provided.as_bytes(), expected.as_bytes())
}

fn webhook_resource_allowed(notification: &Value, payload: &Value) -> bool {
    let resource = string_key(notification, &["resource"]);
    if resource.is_empty() {
        return false;
    }
    let patterns = string_list_key(
        payload,
        &[
            "acceptedResources",
            "accepted_resources",
            "msgraphAcceptedResources",
            "msgraph_accepted_resources",
        ],
    )
    .or_else(|| {
        non_empty_env("MSGRAPH_WEBHOOK_ACCEPTED_RESOURCES")
            .or_else(|| non_empty_env("HERMES_MESSAGING_GATEWAY_MSGRAPH_ACCEPTED_RESOURCES"))
            .map(|raw| split_csv(&raw))
    })
    .unwrap_or_default();
    if patterns.is_empty() {
        return true;
    }
    let normalized_resource = normalize_resource(&resource);
    patterns.into_iter().any(|pattern| {
        let normalized_pattern = normalize_resource(&pattern);
        if normalized_pattern.is_empty() {
            return false;
        }
        if let Some(prefix) = normalized_pattern.strip_suffix('*') {
            let prefix = prefix.trim_end_matches('/');
            normalized_resource == prefix || normalized_resource.starts_with(prefix)
        } else {
            normalized_resource == normalized_pattern
                || normalized_resource.starts_with(&format!("{normalized_pattern}/"))
        }
    })
}

fn create_job_from_notification(
    state: &mut Value,
    notification: &Value,
    receipt_key: &str,
) -> Value {
    if let Some(existing) = find_job_by_dedupe_key(state, receipt_key) {
        return existing;
    }
    let resource_data = notification
        .get("resourceData")
        .or_else(|| notification.get("resource_data"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let meeting_id = string_key(&resource_data, &["id"])
        .if_empty_then(|| string_key(notification, &["meetingId", "meeting_id"]))
        .if_empty_then(|| {
            extract_meeting_id_from_resource(&string_key(notification, &["resource"]))
                .unwrap_or_else(|| string_key(notification, &["resource"]))
        })
        .if_empty_then(|| receipt_key.to_string());
    let job_id = format!("teams-job-{}", stable_short_hash(receipt_key));
    let job = json!({
        "job_id": job_id,
        "event_id": receipt_key,
        "source_event_type": string_key(notification, &["changeType", "change_type"]).if_empty_then(|| "graph.notification".into()),
        "dedupe_key": receipt_key,
        "status": "received",
        "meeting_ref": {
            "meeting_id": meeting_id,
            "tenant_id": string_key(&resource_data, &["tenantId", "tenant_id"]).if_empty_then(|| string_key(notification, &["tenantId", "tenant_id"])),
            "metadata": {
                "notification": notification.clone(),
                "join_web_url": string_key(&resource_data, &["joinWebUrl", "join_web_url"]),
                "call_record_id": string_key(&resource_data, &["callRecordId", "call_record_id"]).if_empty_then(|| string_key(notification, &["callRecordId", "call_record_id"])),
            }
        }
    });
    upsert_object_record(state, "jobs", &job_id, job, "job_id")
}

fn record_receipt_in_state(
    state: &mut Value,
    receipt_key: &str,
    payload: &Value,
    received_at: String,
) {
    let receipts = state
        .get_mut("notification_receipts")
        .and_then(Value::as_object_mut)
        .expect("store state initializes notification_receipts");
    receipts.insert(
        receipt_key.into(),
        json!({"received_at": received_at, "payload": payload}),
    );
}

fn find_job_by_dedupe_key(state: &Value, dedupe_key: &str) -> Option<Value> {
    state
        .get("jobs")
        .and_then(Value::as_object)
        .and_then(|jobs| {
            jobs.values()
                .find(|job| string_key(job, &["dedupe_key", "dedupeKey"]) == dedupe_key)
                .cloned()
        })
}

fn store_contains_key(state: &Value, section: &str, key: &str) -> bool {
    state
        .get(section)
        .and_then(Value::as_object)
        .map(|items| items.contains_key(key))
        .unwrap_or(false)
}

fn extract_meeting_id_from_resource(resource: &str) -> Option<String> {
    let normalized = normalize_resource(resource);
    normalized
        .split('/')
        .rev()
        .find(|part| !part.trim().is_empty())
        .map(str::to_string)
}

fn normalize_resource(resource: &str) -> String {
    resource.trim().trim_matches('/').to_string()
}

fn stable_short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
        .chars()
        .take(12)
        .collect()
}

fn receipt_key_from_payload(payload: &Value) -> String {
    string_key(payload, &["receiptKey", "receipt_key", "key"]).if_empty_then(|| {
        let notification = payload
            .get("notification")
            .or_else(|| payload.get("payload"))
            .cloned()
            .unwrap_or_else(|| payload.clone());
        build_notification_receipt_key(&notification)
    })
}

fn build_notification_receipt_key(notification: &Value) -> String {
    if let Some(id) = notification.get("id").and_then(Value::as_str) {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return format!("id:{trimmed}");
        }
    }
    let canonical =
        serde_json::to_string(notification).unwrap_or_else(|_| notification.to_string());
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn store_stats(state: &Option<Value>) -> Value {
    json!({
        "subscriptions": object_len(state, "subscriptions"),
        "notificationReceipts": object_len(state, "notification_receipts"),
        "eventTimestamps": object_len(state, "event_timestamps"),
        "jobs": object_len(state, "jobs"),
        "sinkRecords": object_len(state, "sink_records"),
    })
}

fn object_len(state: &Option<Value>, key: &str) -> usize {
    state
        .as_ref()
        .and_then(|state| state.get(key))
        .and_then(Value::as_object)
        .map(Map::len)
        .unwrap_or(0)
}

fn object_entries(state: &Option<Value>, key: &str) -> Vec<Value> {
    state
        .as_ref()
        .and_then(|state| state.get(key))
        .and_then(Value::as_object)
        .map(|object| object.values().cloned().collect())
        .unwrap_or_default()
}

fn store_section_keys(state: &Value, key: &str) -> HashSet<String> {
    state
        .get(key)
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn compact_job(mut job: Value) -> Value {
    if let Some(summary) = job
        .get_mut("summary_payload")
        .and_then(Value::as_object_mut)
    {
        if let Some(transcript) = summary
            .remove("transcript_text")
            .or_else(|| summary.remove("transcriptText"))
        {
            if let Some(text) = transcript.as_str() {
                summary.insert(
                    "transcript_preview".into(),
                    json!(text.chars().take(240).collect::<String>()),
                );
            }
        }
    }
    job
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn string_key(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            if key.contains('.') {
                nested_value(value, key).and_then(Value::as_str)
            } else {
                value.get(*key).and_then(Value::as_str)
            }
        })
        .unwrap_or("")
        .trim()
        .to_string()
}

fn null_if_empty(value: String) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn string_list_key(value: &Value, keys: &[&str]) -> Option<Vec<String>> {
    keys.iter().find_map(|key| {
        let value = if key.contains('.') {
            nested_value(value, key)
        } else {
            value.get(*key)
        }?;
        if let Some(items) = value.as_array() {
            let strings = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            return Some(strings);
        }
        value.as_str().map(split_csv)
    })
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn nested_value<'a>(value: &'a Value, dotted_key: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in dotted_key.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn object_key(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| {
        let candidate = if key.contains('.') {
            nested_value(value, key)
        } else {
            value.get(*key)
        }?;
        candidate.as_object()?;
        Some(candidate.clone())
    })
}

fn bool_key(value: &Value, keys: &[&str]) -> bool {
    bool_key_opt(value, keys).unwrap_or(false)
}

fn bool_key_opt(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let value = if key.contains('.') {
            nested_value(value, key)
        } else {
            value.get(*key)
        }?;
        value.as_bool().or_else(|| {
            value
                .as_str()
                .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => Some(true),
                    "0" | "false" | "no" | "off" => Some(false),
                    _ => None,
                })
        })
    })
}

fn int_key(value: &Value, keys: &[&str], default: i64) -> i64 {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_i64)
                .or_else(|| {
                    value
                        .get(*key)
                        .and_then(Value::as_u64)
                        .map(|value| value as i64)
                })
                .or_else(|| {
                    value
                        .get(*key)
                        .and_then(Value::as_str)
                        .and_then(|value| value.trim().parse::<i64>().ok())
                })
        })
        .unwrap_or(default)
}

fn env_present(name: &str) -> bool {
    non_empty_env(name).is_some()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

trait EmptyStringExt {
    fn if_empty_then<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String;
}

impl EmptyStringExt for String {
    fn if_empty_then<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String,
    {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}
