use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, AgentRunRecord, ChatMessage, Conversation, Persona, ToolTraceEntry},
    store::{AppStore, ManagedProcess, ManagedProcessNotificationState},
};

use super::{
    kanban_bulk_update_tool, kanban_comment_tool, kanban_create_tool, kanban_decompose_tool,
    kanban_delete_tool, kanban_link_tool, kanban_specify_tool, kanban_unlink_tool,
    kanban_update_tool,
};

const ACHIEVEMENT_COUNT: u64 = 60;
const ACHIEVEMENT_SNAPSHOT_TTL_SECONDS: u64 = 120;

pub(super) fn dashboard_plugins_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let plugin = payload
        .get("plugin")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .trim()
        .to_ascii_lowercase();
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "status"
            | "list"
            | "manifest"
            | "routes"
            | "achievements"
            | "state"
            | "diagnostics"
            | "rescan"
            | "reset-state"
            | "scan-status"
            | "recent-unlocks"
            | "session-badges"
            | "fastapi-host"
            | "dashboard-host"
            | "host-plan"
            | "host-run"
            | "host-start"
            | "host-stop"
            | "host-restart"
            | "kanban-board"
            | "kanban-config"
            | "kanban-stats"
            | "kanban-assignees"
            | "kanban-task"
            | "kanban-events"
            | "kanban-events-checkpoint"
            | "kanban-runtime-events"
            | "kanban-runtime-checkpoint"
            | "kanban-create"
            | "kanban-update"
            | "kanban-delete"
            | "kanban-comment"
            | "kanban-link"
            | "kanban-unlink"
            | "kanban-bulk"
            | "kanban-specify"
            | "kanban-decompose"
            | "kanban-home-channels"
            | "kanban-home-subscribe"
            | "kanban-home-unsubscribe"
            | "kanban-diagnostics"
            | "kanban-workers-active"
            | "kanban-run"
            | "kanban-run-inspect"
            | "kanban-run-terminate"
            | "kanban-task-log"
            | "kanban-attachments"
            | "kanban-attachment-add"
            | "kanban-attachment-read"
            | "kanban-attachment-delete"
            | "kanban-dispatch"
            | "kanban-reclaim"
            | "kanban-reassign"
            | "kanban-boards"
            | "kanban-board-create"
            | "kanban-board-update"
            | "kanban-board-delete"
            | "kanban-board-switch"
            | "kanban-profiles"
            | "kanban-profile-update"
            | "kanban-profile-describe-auto"
            | "kanban-orchestration"
            | "kanban-orchestration-set"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_dashboard_plugins_desktop_v1",
            "status": "unsupported_action",
            "supportedActions": ["status", "list", "manifest", "routes", "achievements", "state", "diagnostics", "rescan", "reset-state", "scan-status", "recent-unlocks", "session-badges", "fastapi-host", "dashboard-host", "host-plan", "host-run", "host-start", "host-stop", "host-restart", "kanban-board", "kanban-config", "kanban-stats", "kanban-assignees", "kanban-task", "kanban-events", "kanban-events-checkpoint", "kanban-runtime-events", "kanban-runtime-checkpoint", "kanban-create", "kanban-update", "kanban-delete", "kanban-comment", "kanban-link", "kanban-unlink", "kanban-bulk", "kanban-specify", "kanban-decompose", "kanban-home-channels", "kanban-home-subscribe", "kanban-home-unsubscribe", "kanban-diagnostics", "kanban-workers-active", "kanban-run", "kanban-run-inspect", "kanban-run-terminate", "kanban-task-log", "kanban-attachments", "kanban-attachment-add", "kanban-attachment-read", "kanban-attachment-delete", "kanban-dispatch", "kanban-reclaim", "kanban-reassign", "kanban-boards", "kanban-board-create", "kanban-board-update", "kanban-board-delete", "kanban-board-switch", "kanban-profiles", "kanban-profile-update", "kanban-profile-describe-auto", "kanban-orchestration", "kanban-orchestration-set"],
        }))?);
    }

    if matches!(
        action.as_str(),
        "fastapi-host"
            | "dashboard-host"
            | "host-plan"
            | "host-run"
            | "host-start"
            | "host-stop"
            | "host-restart"
    ) {
        return Ok(serde_json::to_string_pretty(
            &dashboard_fastapi_host_process_plan_action(payload),
        )?);
    }
    if action == "achievements" {
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_payload(store)?,
        )?);
    }
    if action == "rescan" {
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_rescan(store)?,
        )?);
    }
    if action == "reset-state" {
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_reset_state(store)?,
        )?);
    }
    if action == "scan-status" {
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_scan_status(store),
        )?);
    }
    if action == "recent-unlocks" {
        let limit = payload.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_recent_unlocks(store, limit),
        )?);
    }
    if action == "session-badges" {
        let session_id = payload
            .get("sessionId")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(serde_json::to_string_pretty(
            &dashboard_achievements_session_badges(store, session_id),
        )?);
    }
    if action == "kanban-board" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_board(
            store, payload,
        )?)?);
    }
    if action == "kanban-config" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_config(
            store,
        )?)?);
    }
    if action == "kanban-stats" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_stats(
            store,
        )?)?);
    }
    if action == "kanban-assignees" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_assignees(
            store,
        )?)?);
    }
    if action == "kanban-task" {
        let task_id = payload
            .get("taskId")
            .or_else(|| payload.get("task_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_task(
            store, task_id,
        )?)?);
    }
    if action == "kanban-events" || action == "kanban-events-checkpoint" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_events(
            store, payload, &action,
        )?)?);
    }
    if action == "kanban-runtime-events" || action == "kanban-runtime-checkpoint" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_runtime_events(store, payload, &action)?,
        )?);
    }
    if action == "kanban-create" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_create_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-update" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_update_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-delete" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_delete_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-comment" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_comment_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-link" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_link_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-unlink" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_unlink_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-bulk" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_write_response(&action, kanban_bulk_update_tool(store, payload)?)?,
        )?);
    }
    if action == "kanban-specify" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_specify(
            store, payload,
        )?)?);
    }
    if action == "kanban-decompose" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_decompose(
            store, payload,
        )?)?);
    }
    if action == "kanban-home-channels" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_home_channels(store, payload)?,
        )?);
    }
    if action == "kanban-home-subscribe" || action == "kanban-home-unsubscribe" {
        let subscribe = action == "kanban-home-subscribe";
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_home_subscription(store, payload, subscribe)?,
        )?);
    }
    if action == "kanban-diagnostics" {
        let severity = payload.get("severity").and_then(Value::as_str);
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_diagnostics(store, severity)?,
        )?);
    }
    if action == "kanban-workers-active" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_workers_active(store)?,
        )?);
    }
    if action == "kanban-run" {
        let run_id = payload
            .get("runId")
            .or_else(|| payload.get("run_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_run(
            store, run_id,
        )?)?);
    }
    if action == "kanban-run-inspect" {
        let run_id = payload
            .get("runId")
            .or_else(|| payload.get("run_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_run_inspect(store, run_id)?,
        )?);
    }
    if action == "kanban-run-terminate" {
        let run_id = payload
            .get("runId")
            .or_else(|| payload.get("run_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reason = payload.get("reason").and_then(Value::as_str);
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_run_terminate(store, run_id, reason)?,
        )?);
    }
    if action == "kanban-task-log" {
        let task_id = payload
            .get("taskId")
            .or_else(|| payload.get("task_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let limit = payload.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_task_log(
            store, task_id, limit,
        )?)?);
    }
    if action == "kanban-attachments" {
        let task_id = payload
            .get("taskId")
            .or_else(|| payload.get("task_id"))
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_attachments(store, task_id)?,
        )?);
    }
    if action == "kanban-attachment-add" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_attachment_add(store, payload)?,
        )?);
    }
    if action == "kanban-attachment-read" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_attachment_read(store, payload)?,
        )?);
    }
    if action == "kanban-attachment-delete" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_attachment_delete(store, payload)?,
        )?);
    }
    if action == "kanban-dispatch" {
        let max_spawn = payload
            .get("maxSpawn")
            .or_else(|| payload.get("max_spawn"))
            .or_else(|| payload.get("max"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let dry_run = payload
            .get("dryRun")
            .or_else(|| payload.get("dry_run"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let enqueue_agent = payload
            .get("enqueueAgent")
            .or_else(|| payload.get("enqueue_agent"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_dispatch(
            store,
            payload,
            max_spawn,
            dry_run,
            enqueue_agent,
        )?)?);
    }
    if action == "kanban-reclaim" {
        let task_id = payload_string(payload, &["taskId", "task_id", "id"]).unwrap_or_default();
        let reason = payload
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_reclaim(
            store, &task_id, reason,
        )?)?);
    }
    if action == "kanban-reassign" {
        let task_id = payload_string(payload, &["taskId", "task_id", "id"]).unwrap_or_default();
        let profile = payload
            .get("profile")
            .or_else(|| payload.get("assignee"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && !matches!(value.to_ascii_lowercase().as_str(), "none" | "-" | "null")
            });
        let reclaim_first = payload
            .get("reclaimFirst")
            .or_else(|| payload.get("reclaim_first"))
            .or_else(|| payload.get("reclaim"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reason = payload
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_reassign(
            store,
            &task_id,
            profile,
            reclaim_first,
            reason,
        )?)?);
    }
    if action == "kanban-boards" {
        let include_archived = payload
            .get("includeArchived")
            .or_else(|| payload.get("include_archived"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_boards(
            store,
            include_archived,
        )?)?);
    }
    if action == "kanban-board-create" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_board_create(store, payload)?,
        )?);
    }
    if action == "kanban-board-update" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_board_update(store, payload)?,
        )?);
    }
    if action == "kanban-board-delete" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_board_delete(store, payload)?,
        )?);
    }
    if action == "kanban-board-switch" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_board_switch(store, payload)?,
        )?);
    }
    if action == "kanban-profiles" {
        return Ok(serde_json::to_string_pretty(&kanban_dashboard_profiles(
            store,
        )?)?);
    }
    if action == "kanban-profile-update" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_profile_update(store, payload)?,
        )?);
    }
    if action == "kanban-profile-describe-auto" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_profile_describe_auto(store, payload)?,
        )?);
    }
    if action == "kanban-orchestration" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_orchestration(store)?,
        )?);
    }
    if action == "kanban-orchestration-set" {
        return Ok(serde_json::to_string_pretty(
            &kanban_dashboard_orchestration_set(store, payload)?,
        )?);
    }

    let plugins = hermes_dashboard_plugins(store)
        .into_iter()
        .filter(|item| {
            plugin == "all"
                || item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(&plugin))
        })
        .collect::<Vec<_>>();

    let snapshot = dashboard_plugins_snapshot(store, plugins);
    Ok(serde_json::to_string_pretty(&snapshot)?)
}

pub(super) fn dashboard_plugins_snapshot(store: &AppStore, plugins: Vec<Value>) -> Value {
    let hermes_home = hermes_home(store);
    let kanban_task_count = store
        .agent_kanban_tasks()
        .map(|tasks| tasks.len())
        .unwrap_or(0);
    let achievements_dir = hermes_home.join("plugins").join("hermes-achievements");
    let state_path = achievements_dir.join("state.json");
    let snapshot_path = achievements_dir.join("scan_snapshot.json");
    let checkpoint_path = achievements_dir.join("scan_checkpoint.json");
    let state = read_json_file(&state_path);
    let snapshot = read_json_file(&snapshot_path);
    let checkpoint = read_json_file(&checkpoint_path);
    let unlock_count = state
        .as_ref()
        .and_then(|value| value.get("unlocks"))
        .and_then(Value::as_object)
        .map(|value| value.len())
        .unwrap_or(0);
    let snapshot_total = snapshot
        .as_ref()
        .and_then(|value| value.get("total_count"))
        .and_then(Value::as_u64);
    let checkpoint_sessions = checkpoint
        .as_ref()
        .and_then(|value| value.get("sessions"))
        .and_then(Value::as_object)
        .map(|value| value.len())
        .unwrap_or(0);
    let tab_runtime = dashboard_plugin_tab_runtime_contract(&plugins);

    json!({
        "schema": "hermes_dashboard_plugins_desktop_v1",
        "status": "ok",
        "count": plugins.len(),
        "plugins": plugins,
        "dashboardHost": {
            "hermesReference": "Hermes dashboard discovers dashboard/manifest.json bundles and mounts plugin_api.py routers under /api/plugins/{plugin}/.",
            "synthChatDesktopEmbeddedHost": true,
            "nativeApiServerManifestRoute": "/api/dashboard/plugins",
            "nativeApiServerAssetRoute": "/dashboard-plugins/{plugin}/{file_path}",
            "nativeApiServerDynamicHttpRoutes": true,
            "native_api_server_dynamic_http_routes": true,
            "dynamicHttpRunner": dashboard_plugin_dynamic_http_runner_contract(),
            "tabRuntime": tab_runtime.clone(),
            "tab_runtime": tab_runtime,
            "fastApiHostProcessPlan": dashboard_fastapi_host_managed_process_plan(),
            "fastapi_host_process_plan": dashboard_fastapi_host_managed_process_plan(),
            "serviceManagerBoundary": dashboard_fastapi_host_service_manager_boundary(),
            "service_manager_boundary": dashboard_fastapi_host_service_manager_boundary(),
            "assetAllowlist": [".js", ".mjs", ".css", ".json", ".html", ".svg", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".woff2", ".woff", ".ttf", ".otf", ".map"],
            "boundary": "SynthChat exposes dashboard plugin discovery, API bridges, Kanban event WebSocket, browser-fetchable dashboard bundle assets, and bounded dynamic HTTP execution for trusted plugin_api.py route handlers through the native API server. It still does not host arbitrary long-lived plugin WebSocket handlers, true long-lived HTTP byte-stream ownership for large/continuous responses, or the full Hermes FastAPI SPA/tab process model."
        },
        "achievements": {
            "plugin": "hermes-achievements",
            "catalogCount": ACHIEVEMENT_COUNT,
            "snapshotTtlSeconds": ACHIEVEMENT_SNAPSHOT_TTL_SECONDS,
            "categories": [
                "Agent Autonomy",
                "Debugging Chaos",
                "Vibe Coding",
                "Hermes Native",
                "Research/Web",
                "Tool Mastery",
                "Model Lore",
                "Lifestyle"
            ],
            "apiRoutes": [
                {"method": "GET", "path": "/api/plugins/hermes-achievements/achievements", "purpose": "returns achievements, counts, stale flag, and scan status"},
                {"method": "GET", "path": "/api/plugins/hermes-achievements/scan-status", "purpose": "returns current scan lifecycle metadata"},
                {"method": "GET", "path": "/api/plugins/hermes-achievements/recent-unlocks", "purpose": "returns latest unlocked achievements"},
                {"method": "GET", "path": "/api/plugins/hermes-achievements/sessions/{session_id}/badges", "purpose": "returns badges for one session"},
                {"method": "POST", "path": "/api/plugins/hermes-achievements/rescan", "purpose": "forces synchronous dashboard scan"},
                {"method": "POST", "path": "/api/plugins/hermes-achievements/reset-state", "purpose": "clears unlock state and scan files"}
            ],
            "state": {
                "hermesHome": hermes_home.to_string_lossy().to_string(),
                "statePath": state_path.to_string_lossy().to_string(),
                "snapshotPath": snapshot_path.to_string_lossy().to_string(),
                "checkpointPath": checkpoint_path.to_string_lossy().to_string(),
                "stateExists": state.is_some(),
                "snapshotExists": snapshot.is_some(),
                "checkpointExists": checkpoint.is_some(),
                "unlockCount": unlock_count,
                "snapshotTotalCount": snapshot_total,
                "checkpointSessionCount": checkpoint_sessions
            },
            "desktopAdaptation": {
                "nativeCatalogSurface": true,
                "nativeHistoryScan": true,
                "nativeUnlockMutation": true,
                "nativeActions": ["rescan", "reset-state", "recent-unlocks", "session-badges"],
                "scanSource": "SynthChat AppStore conversations, messages, agent_runs, and tool_traces",
                "boundary": "SynthChat now performs a desktop-native achievement scan over local conversation/run/tool history and writes Hermes-layout state.json, scan_snapshot.json, and scan_checkpoint.json. It does not embed the FastAPI dashboard host or stream background partial scans."
            }
        },
        "exampleDashboard": {
            "plugin": "example",
            "apiRoutes": [
                {"method": "GET", "path": "/api/plugins/example/hello", "purpose": "stable side-effect-free auth/API-route smoke endpoint"}
            ],
            "expectedHelloPayload": {
                "message": "Hello from the example plugin!",
                "plugin": "example",
                "version": "1.0.0"
            },
            "desktopAdaptation": {
                "nativeRoute": true,
                "boundary": "The example dashboard plugin smoke endpoint is adapted natively with the Hermes hello payload; it is retained as dashboard plugin-host/API auth coverage evidence, not as an agent model tool."
            }
        },
        "kanbanDashboard": {
            "plugin": "kanban",
            "manifest": {
                "name": "kanban",
                "label": "Kanban",
                "version": "1.0.0",
                "tab": {"path": "/kanban", "position": "after:skills"},
                "entry": "dist/index.js",
                "css": "dist/style.css",
                "api": "plugin_api.py"
            },
            "apiRouteGroups": [
                {"group": "board", "routes": ["GET /api/plugins/kanban/board", "GET /api/plugins/kanban/stats", "GET /api/plugins/kanban/assignees", "GET /api/plugins/kanban/boards", "POST /api/plugins/kanban/boards", "PATCH /api/plugins/kanban/boards/{slug}", "DELETE /api/plugins/kanban/boards/{slug}", "POST /api/plugins/kanban/boards/{slug}/switch"]},
                {"group": "tasks", "routes": ["GET /api/plugins/kanban/tasks/{task_id}", "POST /api/plugins/kanban/tasks", "PATCH /api/plugins/kanban/tasks/{task_id}", "DELETE /api/plugins/kanban/tasks/{task_id}", "POST /api/plugins/kanban/tasks/bulk", "POST /api/plugins/kanban/tasks/{task_id}/specify", "POST /api/plugins/kanban/tasks/{task_id}/decompose", "POST /api/plugins/kanban/tasks/{task_id}/reassign", "POST /api/plugins/kanban/tasks/{task_id}/reclaim"]},
                {"group": "collaboration", "routes": ["POST /api/plugins/kanban/tasks/{task_id}/comments", "POST /api/plugins/kanban/links", "DELETE /api/plugins/kanban/links", "GET /api/plugins/kanban/tasks/{task_id}/attachments", "POST /api/plugins/kanban/tasks/{task_id}/attachments", "GET /api/plugins/kanban/attachments/{attachment_id}", "DELETE /api/plugins/kanban/attachments/{attachment_id}"]},
                {"group": "workers", "routes": ["GET /api/plugins/kanban/workers/active", "GET /api/plugins/kanban/runs/{run_id}", "GET /api/plugins/kanban/runs/{run_id}/inspect", "POST /api/plugins/kanban/runs/{run_id}/terminate", "GET /api/plugins/kanban/tasks/{task_id}/log", "POST /api/plugins/kanban/dispatch"]},
                {"group": "notifications", "routes": ["GET /api/plugins/kanban/home-channels", "POST /api/plugins/kanban/tasks/{task_id}/home-subscribe/{platform}", "DELETE /api/plugins/kanban/tasks/{task_id}/home-subscribe/{platform}"]},
                {"group": "orchestration", "routes": ["GET /api/plugins/kanban/profiles", "PATCH /api/plugins/kanban/profiles/{profile_name}", "POST /api/plugins/kanban/profiles/{profile_name}/describe-auto", "GET /api/plugins/kanban/orchestration", "PUT /api/plugins/kanban/orchestration", "GET /api/plugins/kanban/diagnostics", "WebSocket /api/plugins/kanban/events"]}
            ],
            "nativeDashboardReadActions": ["kanban-board", "kanban-stats", "kanban-assignees", "kanban-task"],
            "nativeEventStreamActions": ["kanban-events", "kanban-events-checkpoint", "kanban-runtime-events", "kanban-runtime-checkpoint"],
            "nativeRuntimeEventSources": ["agent_queue", "agent_runs", "agent_run.phase_events", "agent_run.tool_events", "managed_processes", "task.events"],
            "nativeDashboardWriteActions": ["kanban-create", "kanban-update", "kanban-delete", "kanban-comment", "kanban-link", "kanban-unlink", "kanban-bulk"],
            "nativeWorkerVisibilityActions": ["kanban-diagnostics", "kanban-workers-active", "kanban-run", "kanban-run-inspect", "kanban-run-terminate", "kanban-task-log", "kanban-attachments", "kanban-dispatch", "kanban-reclaim", "kanban-reassign"],
            "nativeAttachmentActions": ["kanban-attachments", "kanban-attachment-add", "kanban-attachment-read", "kanban-attachment-delete"],
            "nativeBoardOrchestrationActions": ["kanban-boards", "kanban-board-create", "kanban-board-update", "kanban-board-delete", "kanban-board-switch", "kanban-profiles", "kanban-profile-update", "kanban-profile-describe-auto", "kanban-orchestration", "kanban-orchestration-set"],
            "synthChatNativeTools": [
                "kanban_create",
                "kanban_decompose",
                "kanban_specify",
                "kanban_list",
                "kanban_show",
                "kanban_update",
                "kanban_delete",
                "kanban_complete",
                "kanban_block",
                "kanban_unblock",
                "kanban_heartbeat",
                "kanban_comment",
                "kanban_link",
                "kanban_unlink",
                "kanban_bulk_update"
            ],
            "state": {
                "nativeTaskCount": kanban_task_count,
                "synthChatState": "AppStore.agent_kanban_tasks",
                "hermesReferenceDb": "HERMES_HOME/kanban/kanban.db",
                "hermesWorkerLogs": "HERMES_HOME/kanban/logs/<task>.log"
            },
            "dispatcher": {
                "hermesDefault": "gateway_embedded",
                "configKey": "kanban.dispatch_in_gateway",
                "deprecatedStandaloneService": "plugins/kanban/systemd/hermes-kanban-dispatcher.service",
                "standaloneCommand": "hermes kanban daemon --force --interval 60 --pidfile %t/hermes-kanban-dispatcher.pid",
                "boundary": "Hermes dashboard can dispatch, inspect, reclaim, terminate, and stream DB-backed workers through the FastAPI dashboard host. SynthChat adapts the agent-facing kanban task tools, slash command flow, board/stats/assignee/task dashboard payloads, task mutations, AppStore-backed task event cursor polling plus native API WebSocket /api/plugins/kanban/events, merged queue/run/tool/process runtime cursor polling, desktop file-backed task attachments, ManagedProcess-backed worker visibility/termination, and recovery reclaim/reassign actions natively; the remaining boundary is the full embedded FastAPI dashboard host/tab runtime rather than the Kanban event WebSocket itself."
            }
        }
    })
}

fn dashboard_fastapi_host_managed_process_plan() -> Value {
    json!({
        "schema": "hermes_dashboard_fastapi_host_managed_process_plan_desktop_v1",
        "taskId": "hermes-dashboard-fastapi-host",
        "task_id": "hermes-dashboard-fastapi-host",
        "command": "hermes dashboard --no-open",
        "hermesCommand": "hermes dashboard",
        "hermes_command": "hermes dashboard",
        "defaultUrl": "http://127.0.0.1:9119",
        "default_url": "http://127.0.0.1:9119",
        "statusCommand": "hermes dashboard --status",
        "status_command": "hermes dashboard --status",
        "stopCommand": "hermes dashboard --stop",
        "stop_command": "hermes dashboard --stop",
        "tuiCommand": "hermes dashboard --tui --no-open",
        "tui_command": "hermes dashboard --tui --no-open",
        "optionalArgs": ["--host <host>", "--port <port>", "--tui", "--insecure"],
        "optional_args": ["--host <host>", "--port <port>", "--tui", "--insecure"],
        "managedProcessStartPayload": {
            "action": "start",
            "label": "Hermes dashboard FastAPI host",
            "command": "hermes dashboard --no-open",
            "taskId": "hermes-dashboard-fastapi-host",
            "notifyOnComplete": true,
            "watchPatterns": ["Uvicorn", "dashboard", "http://127.0.0.1:9119", "error"]
        },
        "managed_process_start_payload": {
            "action": "start",
            "label": "Hermes dashboard FastAPI host",
            "command": "hermes dashboard --no-open",
            "taskId": "hermes-dashboard-fastapi-host",
            "task_id": "hermes-dashboard-fastapi-host",
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["Uvicorn", "dashboard", "http://127.0.0.1:9119", "error"],
            "watch_patterns": ["Uvicorn", "dashboard", "http://127.0.0.1:9119", "error"]
        },
        "managedProcessStopPayload": {
            "action": "start",
            "label": "Hermes dashboard stop",
            "command": "hermes dashboard --stop",
            "taskId": "hermes-dashboard-fastapi-host-stop",
            "notifyOnComplete": true,
            "watchPatterns": ["Stopping", "No hermes dashboard", "remaining"]
        },
        "managedProcessTaskStopPayload": {
            "action": "stop_all",
            "taskId": "hermes-dashboard-fastapi-host",
            "forget": false
        },
        "managed_process_stop_payload": {
            "action": "start",
            "label": "Hermes dashboard stop",
            "command": "hermes dashboard --stop",
            "taskId": "hermes-dashboard-fastapi-host-stop",
            "task_id": "hermes-dashboard-fastapi-host-stop",
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["Stopping", "No hermes dashboard", "remaining"],
            "watch_patterns": ["Stopping", "No hermes dashboard", "remaining"]
        },
        "managed_process_task_stop_payload": {
            "action": "stop_all",
            "taskId": "hermes-dashboard-fastapi-host",
            "task_id": "hermes-dashboard-fastapi-host",
            "forget": false
        },
        "boundary": "This plan lets SynthChat's existing managed-process tool start or stop the external Hermes FastAPI dashboard host. It does not embed the Hermes SPA/tab shell or arbitrary plugin frontend/runtime inside the Tauri process."
    })
}

fn dashboard_fastapi_host_process_plan_action(payload: &Value) -> Value {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("fastapi-host")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    let execute_requested = payload
        .get("execute")
        .or_else(|| payload.get("live"))
        .or_else(|| payload.get("apply"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let command_override = payload
        .get("dashboardCommand")
        .or_else(|| payload.get("dashboard_command"))
        .or_else(|| payload.get("hostCommand"))
        .or_else(|| payload.get("host_command"))
        .or_else(|| payload.get("command"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut plan = dashboard_fastapi_host_managed_process_plan();
    if let Some(command) = command_override {
        plan["command"] = json!(command.clone());
        plan["managedProcessStartPayload"]["command"] = json!(command.clone());
        plan["managed_process_start_payload"]["command"] = json!(command);
    }
    json!({
        "schema": "hermes_dashboard_fastapi_host_action_desktop_v1",
        "action": action,
        "status": if execute_requested { "managed_process_execution_requested" } else { "fastapi_host_process_plan" },
        "executeRequested": execute_requested,
        "execute_requested": execute_requested,
        "hostProcessPlan": plan.clone(),
        "host_process_plan": plan.clone(),
        "managedProcessPlan": plan.clone(),
        "managed_process_plan": plan,
        "boundary": "This action exposes the Hermes dashboard FastAPI host launch/stop contract directly. Starting the external Hermes dashboard process should go through the async managed-process path so approvals, logs, and stop controls remain visible."
    })
}

fn dashboard_fastapi_host_service_manager_boundary() -> Value {
    json!({
        "schema": "hermes_dashboard_fastapi_host_service_manager_boundary_desktop_v1",
        "hermesReference": "hermes_cli/main.py::cmd_dashboard",
        "hermes_reference": "hermes_cli/main.py::cmd_dashboard",
        "serviceManagerSupportedByHermes": false,
        "service_manager_supported_by_hermes": false,
        "osServiceManagerApplied": false,
        "os_service_manager_applied": false,
        "pidFileLifecycle": false,
        "pid_file_lifecycle": false,
        "processControlCommands": {
            "start": "hermes dashboard --no-open",
            "status": "hermes dashboard --status",
            "stop": "hermes dashboard --stop",
            "tui": "hermes dashboard --tui --no-open"
        },
        "process_control_commands": {
            "start": "hermes dashboard --no-open",
            "status": "hermes dashboard --status",
            "stop": "hermes dashboard --stop",
            "tui": "hermes dashboard --tui --no-open"
        },
        "remainingBoundary": "Hermes dashboard has no systemd/launchd/schtasks/s6 install path; the parity boundary is process supervision and full FastAPI SPA/tab hosting, not a missing dashboard OS service manager.",
        "remaining_boundary": "Hermes dashboard has no systemd/launchd/schtasks/s6 install path; the parity boundary is process supervision and full FastAPI SPA/tab hosting, not a missing dashboard OS service manager."
    })
}

fn dashboard_plugin_tab_runtime_contract(plugins: &[Value]) -> Value {
    let tabs = plugins
        .iter()
        .filter_map(|plugin| {
            let name = plugin.get("name").and_then(Value::as_str)?;
            let tab = plugin.get("tab").cloned().unwrap_or_else(|| json!({}));
            let tab_path = tab
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("/{name}"));
            let entry = plugin
                .get("entry")
                .and_then(Value::as_str)
                .unwrap_or("dist/index.js");
            let css = plugin.get("css").and_then(Value::as_str);
            let css_asset_url = css.map(|path| format!("/dashboard-plugins/{name}/{path}"));
            Some(json!({
                "plugin": name,
                "label": plugin.get("label").cloned().unwrap_or_else(|| json!(name)),
                "path": tab_path,
                "position": tab.get("position").cloned().unwrap_or_else(|| json!("end")),
                "override": tab.get("override").cloned().unwrap_or(Value::Null),
                "hidden": tab.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                "entry": entry,
                "entryAssetUrl": format!("/dashboard-plugins/{name}/{entry}"),
                "entry_asset_url": format!("/dashboard-plugins/{name}/{entry}"),
                "css": css,
                "cssAssetUrl": css_asset_url.clone(),
                "css_asset_url": css_asset_url,
                "apiMount": format!("/api/plugins/{name}"),
                "api_mount": format!("/api/plugins/{name}"),
                "manifestRoute": "/api/dashboard/plugins",
                "assetRoute": format!("/dashboard-plugins/{name}/{{file_path}}"),
                "asset_route": format!("/dashboard-plugins/{name}/{{file_path}}"),
                "desktopHostCanServeAssets": true,
                "desktop_host_can_serve_assets": true,
                "desktopHostCanBridgeApi": true,
                "desktop_host_can_bridge_api": true,
                "nativeTabRendered": false,
                "native_tab_rendered": false
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "schema": "hermes_dashboard_plugin_tab_runtime_desktop_v1",
        "hermesReference": "hermes_cli/web_server.py::_discover_dashboard_plugins and serve_plugin_asset",
        "tabCount": tabs.len(),
        "tab_count": tabs.len(),
        "tabs": tabs,
        "manifestRoute": "/api/dashboard/plugins",
        "manifest_route": "/api/dashboard/plugins",
        "assetRouteTemplate": "/dashboard-plugins/{plugin}/{file_path}",
        "asset_route_template": "/dashboard-plugins/{plugin}/{file_path}",
        "apiRouteTemplate": "/api/plugins/{plugin}/{path}",
        "api_route_template": "/api/plugins/{plugin}/{path}",
        "dashboardAuthApplies": true,
        "dashboard_auth_applies": true,
        "prefixRewriteRequired": true,
        "prefix_rewrite_required": true,
        "viteAbsoluteAssetRewrite": true,
        "vite_absolute_asset_rewrite": true,
        "nativeTabShellEmbedded": false,
        "native_tab_shell_embedded": false,
        "frontendManifestConsumable": true,
        "frontend_manifest_consumable": true,
        "boundary": "SynthChat exposes Hermes dashboard plugin tab manifests, static assets, API mounts, and auth-aware routes for a desktop/web frontend to consume, but does not embed Hermes' FastAPI-served SPA tab shell or execute arbitrary plugin frontend code inside a native Tauri WebView host here."
    })
}

fn dashboard_plugin_dynamic_http_runner_contract() -> Value {
    let mut value = json!({
        "schema": "hermes_dashboard_plugin_dynamic_http_runner_desktop_v1",
        "trustedPluginApiImport": true,
        "trusted_plugin_api_import": true,
        "boundedSubprocessExecution": true,
        "bounded_subprocess_execution": true,
        "fastApiRouteDecorators": ["get", "post", "put", "patch", "delete", "head", "options", "api_route"],
        "fastapi_route_decorators": ["get", "post", "put", "patch", "delete", "head", "options", "api_route"],
        "fastApiAppRouteDiscovery": true,
        "fastapi_app_route_discovery": true,
        "dependencyInjection": true,
        "dependency_injection": true,
        "dependencyCache": true,
        "dependency_cache": true,
        "yieldDependencies": true,
        "yield_dependencies": true,
        "securityDependencies": true,
        "security_dependencies": true,
        "requestObject": true,
        "request_object": true,
        "bodyFormMultipart": true,
        "body_form_multipart": true,
        "pydanticBodyModels": true,
        "pydantic_body_models": true,
        "pydanticFallbackModels": true,
        "pydantic_fallback_models": true,
        "httpExceptionPropagation": true,
        "http_exception_propagation": true,
        "responseEnvelopes": ["JSONResponse", "ORJSONResponse", "UJSONResponse", "PlainTextResponse", "HTMLResponse", "FileResponse"],
        "response_envelopes": ["JSONResponse", "ORJSONResponse", "UJSONResponse", "PlainTextResponse", "HTMLResponse", "FileResponse"],
        "smallFileResponseBodyBase64": true,
        "small_file_response_body_base64": true,
        "websocketRouteDiscovery": true,
        "websocket_route_discovery": true,
        "websocketRuntimeBoundary": dashboard_plugin_websocket_runtime_boundary_contract(),
        "websocket_runtime_boundary": dashboard_plugin_websocket_runtime_boundary_contract(),
        "websocketHandlerExecution": false,
        "websocket_handler_execution": false,
        "dependencyOverrides": true,
        "dependency_overrides": true,
        "longLivedFastApiAppRuntime": false,
        "long_lived_fastapi_app_runtime": false
    });
    value["fileResponseContentDisposition"] = json!(true);
    value["file_response_content_disposition"] = json!(true);
    value["fileResponseContentDispositionType"] = json!(true);
    value["file_response_content_disposition_type"] = json!(true);
    value["fileResponseStatHeaders"] = json!(true);
    value["file_response_stat_headers"] = json!(true);
    value["pydanticFallbackNestedModels"] = json!(true);
    value["pydantic_fallback_nested_models"] = json!(true);
    value["pydanticFallbackRecursiveModelDump"] = json!(true);
    value["pydantic_fallback_recursive_model_dump"] = json!(true);
    value["pydanticFallbackNestedDumpFilters"] = json!(true);
    value["pydantic_fallback_nested_dump_filters"] = json!(true);
    value["pydanticFallbackBytesJsonSerialization"] = json!(true);
    value["pydantic_fallback_bytes_json_serialization"] = json!(true);
    value["pydanticFallbackJsonMethods"] = json!(true);
    value["pydantic_fallback_json_methods"] = json!(true);
    value["pydanticFallbackCopyConstruct"] = json!(true);
    value["pydantic_fallback_copy_construct"] = json!(true);
    value["pydanticFallbackJsonSchema"] = json!(true);
    value["pydantic_fallback_json_schema"] = json!(true);
    value["pydanticFallbackFieldSchemaMetadata"] = json!(true);
    value["pydantic_fallback_field_schema_metadata"] = json!(true);
    value["pydanticFallbackModelConfigPopulateByName"] = json!(true);
    value["pydantic_fallback_model_config_populate_by_name"] = json!(true);
    value["pydanticFallbackModelConfigExtra"] = json!(true);
    value["pydantic_fallback_model_config_extra"] = json!(true);
    value["pydanticFallbackSplitAliases"] = json!(true);
    value["pydantic_fallback_split_aliases"] = json!(true);
    value["pydanticFallbackAliasChoices"] = json!(true);
    value["pydantic_fallback_alias_choices"] = json!(true);
    value["pydanticFallbackAliasPath"] = json!(true);
    value["pydantic_fallback_alias_path"] = json!(true);
    value["pydanticFallbackFieldValidators"] = json!(true);
    value["pydantic_fallback_field_validators"] = json!(true);
    value["pydanticFallbackModelValidators"] = json!(true);
    value["pydantic_fallback_model_validators"] = json!(true);
    value["pydanticFallbackValidationInfo"] = json!(true);
    value["pydantic_fallback_validation_info"] = json!(true);
    value["pydanticFallbackFunctionalValidators"] = json!(true);
    value["pydantic_fallback_functional_validators"] = json!(true);
    value["pydanticFallbackUrlSecretTypes"] = json!(true);
    value["pydantic_fallback_url_secret_types"] = json!(true);
    value["pydanticFallbackPrivateAttr"] = json!(true);
    value["pydantic_fallback_private_attr"] = json!(true);
    value["pydanticFallbackTypeAdapter"] = json!(true);
    value["pydantic_fallback_type_adapter"] = json!(true);
    value["pydanticFallbackRootModel"] = json!(true);
    value["pydantic_fallback_root_model"] = json!(true);
    value["pydanticFallbackComputedField"] = json!(true);
    value["pydantic_fallback_computed_field"] = json!(true);
    value["pydanticFallbackFieldSerializer"] = json!(true);
    value["pydantic_fallback_field_serializer"] = json!(true);
    value["pydanticFallbackModelSerializer"] = json!(true);
    value["pydantic_fallback_model_serializer"] = json!(true);
    value["requestAppState"] = json!(true);
    value["request_app_state"] = json!(true);
    value["requestUrlObject"] = json!(true);
    value["request_url_object"] = json!(true);
    value["requestUrlComponents"] = json!(true);
    value["request_url_components"] = json!(true);
    value["requestUrlAbsolute"] = json!(true);
    value["request_url_absolute"] = json!(true);
    value["requestUrlMutationHelpers"] = json!(true);
    value["request_url_mutation_helpers"] = json!(true);
    value["requestUrlFor"] = json!(true);
    value["request_url_for"] = json!(true);
    value["requestUrlForPathConvertor"] = json!(true);
    value["request_url_for_path_convertor"] = json!(true);
    value["appUrlPathFor"] = json!(true);
    value["app_url_path_for"] = json!(true);
    value["requestClientBaseScope"] = json!(true);
    value["request_client_base_scope"] = json!(true);
    value["requestServerScope"] = json!(true);
    value["request_server_scope"] = json!(true);
    value["requestPathParamsScope"] = json!(true);
    value["request_path_params_scope"] = json!(true);
    value["requestRouteScope"] = json!(true);
    value["request_route_scope"] = json!(true);
    value["requestEndpointScope"] = json!(true);
    value["request_endpoint_scope"] = json!(true);
    value["requestAppRouterScope"] = json!(true);
    value["request_app_router_scope"] = json!(true);
    value["requestBaseUrlComponents"] = json!(true);
    value["request_base_url_components"] = json!(true);
    value["requestForwardedPrefixRootPath"] = json!(true);
    value["request_forwarded_prefix_root_path"] = json!(true);
    value["requestBodyStream"] = json!(true);
    value["request_body_stream"] = json!(true);
    value["requestFormData"] = json!(true);
    value["request_form_data"] = json!(true);
    value["requestMultiItems"] = json!(true);
    value["request_multi_items"] = json!(true);
    value["starletteDataStringRepresentations"] = json!(true);
    value["starlette_data_string_representations"] = json!(true);
    value["multipartFormDataGetList"] = json!(true);
    value["multipart_form_data_get_list"] = json!(true);
    value["uploadFileSeekClose"] = json!(true);
    value["upload_file_seek_close"] = json!(true);
    value["uploadFileCloseClosesFileObject"] = json!(true);
    value["upload_file_close_closes_file_object"] = json!(true);
    value["uploadFileWriteSize"] = json!(true);
    value["upload_file_write_size"] = json!(true);
    value["uploadFileHeaders"] = json!(true);
    value["upload_file_headers"] = json!(true);
    value["uploadFileFileObject"] = json!(true);
    value["upload_file_file_object"] = json!(true);
    value["caseInsensitiveRequestHeaders"] = json!(true);
    value["case_insensitive_request_headers"] = json!(true);
    value["starletteHeaderHelpers"] = json!(true);
    value["starlette_header_helpers"] = json!(true);
    value["mutableHeaderAppend"] = json!(true);
    value["mutable_header_append"] = json!(true);
    value["headerConvertUnderscores"] = json!(true);
    value["header_convert_underscores"] = json!(true);
    value["requestCookies"] = json!(true);
    value["request_cookies"] = json!(true);
    value["requestState"] = json!(true);
    value["request_state"] = json!(true);
    value["starletteStateObject"] = json!(true);
    value["starlette_state_object"] = json!(true);
    value["starletteCommonDatastructures"] = json!(true);
    value["starlette_common_datastructures"] = json!(true);
    value["httpMiddleware"] = json!(true);
    value["http_middleware"] = json!(true);
    value["baseHttpMiddleware"] = json!(true);
    value["base_http_middleware"] = json!(true);
    value["baseHttpMiddlewareOptions"] = json!(true);
    value["base_http_middleware_options"] = json!(true);
    value["corsMiddleware"] = json!(true);
    value["cors_middleware"] = json!(true);
    value["boundedAppEventHooks"] = json!(true);
    value["bounded_app_event_hooks"] = json!(true);
    value["boundedLifespanContext"] = json!(true);
    value["bounded_lifespan_context"] = json!(true);
    value["fastApiAppMetadata"] = json!(true);
    value["fastapi_app_metadata"] = json!(true);
    value["fastApiOpenApiSnapshot"] = json!(true);
    value["fastapi_openapi_snapshot"] = json!(true);
    value["fastApiRouteOpenApiMetadata"] = json!(true);
    value["fastapi_route_openapi_metadata"] = json!(true);
    value["exceptionHandlers"] = json!(true);
    value["exception_handlers"] = json!(true);
    value["requestValidationErrorHandlers"] = json!(true);
    value["request_validation_error_handlers"] = json!(true);
    value["pydanticBodyValidationErrors"] = json!(true);
    value["pydantic_body_validation_errors"] = json!(true);
    value["pydanticFallbackPep604Unions"] = json!(true);
    value["pydantic_fallback_pep604_unions"] = json!(true);
    value["pydanticFallbackFieldsSetDumpFilters"] = json!(true);
    value["pydantic_fallback_fields_set_dump_filters"] = json!(true);
    value["addExceptionHandlerRegistration"] = json!(true);
    value["add_exception_handler_registration"] = json!(true);
    value["fastApiIncludeRouterPrefix"] = json!(true);
    value["fastapi_include_router_prefix"] = json!(true);
    value["fastApiIncludeRouterDependencies"] = json!(true);
    value["fastapi_include_router_dependencies"] = json!(true);
    value["fastApiIncludeRouterMetadata"] = json!(true);
    value["fastapi_include_router_metadata"] = json!(true);
    value["fastApiNestedRouterInclude"] = json!(true);
    value["fastapi_nested_router_include"] = json!(true);
    value["fastApiMultilineIncludeRouterScan"] = json!(true);
    value["fastapi_multiline_include_router_scan"] = json!(true);
    value["fastApiMultilineRouteDecoratorScan"] = json!(true);
    value["fastapi_multiline_route_decorator_scan"] = json!(true);
    value["fastApiRouterPrefix"] = json!(true);
    value["fastapi_router_prefix"] = json!(true);
    value["fastApiMultilineRouterPrefixScan"] = json!(true);
    value["fastapi_multiline_router_prefix_scan"] = json!(true);
    value["fastApiPathConvertor"] = json!(true);
    value["fastapi_path_convertor"] = json!(true);
    value["fastApiScalarPathConvertors"] = json!(["str", "int", "float", "uuid"]);
    value["fastapi_scalar_path_convertors"] = json!(["str", "int", "float", "uuid"]);
    value["fastApiUuidPathParamCoercion"] = json!(true);
    value["fastapi_uuid_path_param_coercion"] = json!(true);
    value["fastApiDateTimeParamCoercion"] = json!(true);
    value["fastapi_date_time_param_coercion"] = json!(true);
    value["fastApiAdvancedParamCoercion"] = json!(["Decimal", "Literal", "Enum"]);
    value["fastapi_advanced_param_coercion"] = json!(["Decimal", "Literal", "Enum"]);
    value["fastApiContainerParamCoercion"] = json!(["tuple", "set", "Union"]);
    value["fastapi_container_param_coercion"] = json!(["tuple", "set", "Union"]);
    value["fastApiApiRoute"] = json!(true);
    value["fastapi_api_route"] = json!(true);
    value["fastApiAddApiRoute"] = json!(true);
    value["fastapi_add_api_route"] = json!(true);
    value["fastApiMultilineAddApiRouteScan"] = json!(true);
    value["fastapi_multiline_add_api_route_scan"] = json!(true);
    value["fastApiJsonableEncoder"] = json!(true);
    value["fastapi_jsonable_encoder"] = json!(true);
    value["fastApiJsonableEncoderFiltering"] = json!([
        "include",
        "exclude",
        "exclude_none",
        "exclude_defaults",
        "exclude_unset"
    ]);
    value["fastapi_jsonable_encoder_filtering"] = json!([
        "include",
        "exclude",
        "exclude_none",
        "exclude_defaults",
        "exclude_unset"
    ]);
    value["starletteConcurrencyRunInThreadpool"] = json!(true);
    value["starlette_concurrency_run_in_threadpool"] = json!(true);
    value["fastApiConcurrencyRunInThreadpool"] = json!(true);
    value["fastapi_concurrency_run_in_threadpool"] = json!(true);
    value["parameterRegexConstraints"] = json!(true);
    value["parameter_regex_constraints"] = json!(true);
    value["parameterMetadataFields"] = json!(true);
    value["parameter_metadata_fields"] = json!(true);
    value["annotatedParameterMetadata"] = json!(true);
    value["annotated_parameter_metadata"] = json!(true);
    value["dependencyObjectRepr"] = json!(true);
    value["dependency_object_repr"] = json!(true);
    value["responseParameter"] = json!(true);
    value["response_parameter"] = json!(true);
    value["responseParameterMetadataMerge"] = json!(true);
    value["response_parameter_metadata_merge"] = json!(true);
    value["responseParameterPreservesRouteStatus"] = json!(true);
    value["response_parameter_preserves_route_status"] = json!(true);
    value["responseHeadersCaseInsensitive"] = json!(true);
    value["response_headers_case_insensitive"] = json!(true);
    value["responseRawHeaders"] = json!(true);
    value["response_raw_headers"] = json!(true);
    value["responseMultiValueHeaders"] = json!(true);
    value["response_multi_value_headers"] = json!(true);
    value["responseDefaultHeaders"] = json!(true);
    value["response_default_headers"] = json!(true);
    value["responseRenderNoneEmptyBody"] = json!(true);
    value["response_render_none_empty_body"] = json!(true);
    value["responseRenderMemoryviewBody"] = json!(true);
    value["response_render_memoryview_body"] = json!(true);
    value["backgroundTasksTaskObjects"] = json!(true);
    value["background_tasks_task_objects"] = json!(true);
    value["responseBackgroundTask"] = json!(true);
    value["response_background_task"] = json!(true);
    value["responseBackgroundTasks"] = json!(true);
    value["response_background_tasks"] = json!(true);
    value["starletteResponseImports"] = json!(true);
    value["starlette_response_imports"] = json!(true);
    value["starletteRoutingImports"] = json!(true);
    value["starlette_routing_imports"] = json!(true);
    value["starletteTypesImports"] = json!(true);
    value["starlette_types_imports"] = json!(true);
    value["starletteMiddlewareConfigImport"] = json!(true);
    value["starlette_middleware_config_import"] = json!(true);
    value["testClientImportBoundary"] = json!(true);
    value["testclient_import_boundary"] = json!(true);
    value["fastApiRoutingImports"] = json!(true);
    value["fastapi_routing_imports"] = json!(true);
    value["starletteExceptionImports"] = json!(true);
    value["starlette_exception_imports"] = json!(true);
    value["starletteDatastructureImports"] = json!(true);
    value["starlette_datastructure_imports"] = json!(true);
    value["starletteUrlImports"] = json!(true);
    value["starlette_url_imports"] = json!(true);
    value["starletteUrlPathComponents"] = json!(true);
    value["starlette_url_path_components"] = json!(true);
    value["starletteStaticMiddlewareImports"] = json!(true);
    value["starlette_static_middleware_imports"] = json!(true);
    value["starletteWebSocketImports"] = json!(true);
    value["starlette_websocket_imports"] = json!(true);
    value["starletteWebSocketExceptionImports"] = json!(true);
    value["starlette_websocket_exception_imports"] = json!(true);
    value["starletteExceptionWebSocketImports"] = json!(true);
    value["starlette_exception_websocket_imports"] = json!(true);
    value["fastApiExceptionWebSocketImports"] = json!(true);
    value["fastapi_exception_websocket_imports"] = json!(true);
    value["extendedStatusConstants"] = json!(true);
    value["extended_status_constants"] = json!(true);
    value["webSocketStatusConstants"] = json!(true);
    value["websocket_status_constants"] = json!(true);
    value["starletteWebSocketStatusConstants"] = json!(true);
    value["starlette_websocket_status_constants"] = json!(true);
    value["fastApiSecuritySubmoduleImports"] = json!(true);
    value["fastapi_security_submodule_imports"] = json!(true);
    value["fastApiRequestSubmoduleImports"] = json!(true);
    value["fastapi_request_submodule_imports"] = json!(true);
    value["fastApiDatastructureSubmoduleImports"] = json!(true);
    value["fastapi_datastructure_submodule_imports"] = json!(true);
    value["fastApiBackgroundSubmoduleImports"] = json!(true);
    value["fastapi_background_submodule_imports"] = json!(true);
    value["fastApiSubmoduleParentAttributes"] = json!(true);
    value["fastapi_submodule_parent_attributes"] = json!(true);
    value["routeResponseClass"] = json!(true);
    value["route_response_class"] = json!(true);
    value["routeResponseModel"] = json!(true);
    value["route_response_model"] = json!(true);
    value["routeResponseModelFiltering"] = json!([
        "list_response_model",
        "response_model_include",
        "response_model_exclude",
        "response_model_by_alias",
        "response_model_exclude_none",
        "response_model_exclude_defaults",
        "response_model_exclude_unset"
    ]);
    value["route_response_model_filtering"] = json!([
        "list_response_model",
        "response_model_include",
        "response_model_exclude",
        "response_model_by_alias",
        "response_model_exclude_none",
        "response_model_exclude_defaults",
        "response_model_exclude_unset"
    ]);
    value["responseCookies"] = json!(true);
    value["response_cookies"] = json!(true);
    value["responseDeleteCookie"] = json!(true);
    value["response_delete_cookie"] = json!(true);
    value["redirectResponse"] = json!(true);
    value["redirect_response"] = json!(true);
    value["redirectResponseLocationQuoting"] = json!(true);
    value["redirect_response_location_quoting"] = json!(true);
    value["htmlResponse"] = json!(true);
    value["html_response"] = json!(true);
    value["orjsonUjsonResponse"] = json!(true);
    value["orjson_ujson_response"] = json!(true);
    value["boundedStreamingResponseSnapshot"] = json!(true);
    value["bounded_streaming_response_snapshot"] = json!(true);
    value["boundedStreamingFileResponseSnapshots"] = json!(true);
    value["bounded_streaming_file_response_snapshots"] = json!(true);
    value["streamingResponsePassthrough"] = json!(false);
    value["streaming_response_passthrough"] = json!(false);
    value["streamingResponseBackgroundTask"] = json!(true);
    value["streaming_response_background_task"] = json!(true);
    value["apiKeyHeaderSecurity"] = json!(true);
    value["api_key_header_security"] = json!(true);
    value["apiKeyCookieSecurity"] = json!(true);
    value["api_key_cookie_security"] = json!(true);
    value["apiKeyQuerySecurity"] = json!(true);
    value["api_key_query_security"] = json!(true);
    value["httpBearerSecurity"] = json!(true);
    value["http_bearer_security"] = json!(true);
    value["httpBasicSecurity"] = json!(true);
    value["http_basic_security"] = json!(true);
    value["oauth2PasswordBearerSecurity"] = json!(true);
    value["oauth2_password_bearer_security"] = json!(true);
    value["oauth2AuthorizationCodeBearerSecurity"] = json!(true);
    value["oauth2_authorization_code_bearer_security"] = json!(true);
    value["oauth2PasswordRequestForm"] = json!(true);
    value["oauth2_password_request_form"] = json!(true);
    value["oauth2PasswordRequestFormStrict"] = json!(true);
    value["oauth2_password_request_form_strict"] = json!(true);
    value["openIdConnectSecurity"] = json!(true);
    value["open_id_connect_security"] = json!(true);
    value["fastApiParamDefaultFactory"] = json!(true);
    value["fastapi_param_default_factory"] = json!(true);
    value["staticFilesMount"] = json!(true);
    value["static_files_mount"] = json!(true);
    value["staticFilesHead"] = json!(true);
    value["static_files_head"] = json!(true);
    value["staticFilesMimeType"] = json!(true);
    value["static_files_mime_type"] = json!(true);
    value["fastApiMultilineStaticFilesMountScan"] = json!(true);
    value["fastapi_multiline_static_files_mount_scan"] = json!(true);
    value
}

fn dashboard_plugin_websocket_runtime_boundary_contract() -> Value {
    json!({
        "schema": "hermes_dashboard_plugin_websocket_runtime_boundary_desktop_v1",
        "hermesReferences": [
            "hermes_cli/web_server.py::_mount_plugin_api_routes",
            "hermes_cli/web_server.py::_ws_auth_ok",
            "plugins/kanban/dashboard/plugin_api.py::stream_events"
        ],
        "hermes_references": [
            "hermes_cli/web_server.py::_mount_plugin_api_routes",
            "hermes_cli/web_server.py::_ws_auth_ok",
            "plugins/kanban/dashboard/plugin_api.py::stream_events"
        ],
        "declaredRouteDiscovery": true,
        "declared_route_discovery": true,
        "nativeDeclaredUpgradeBoundary": true,
        "native_declared_upgrade_boundary": true,
        "boundaryFrameSchema": "hermes_dashboard_plugin_websocket_boundary_v1",
        "boundary_frame_schema": "hermes_dashboard_plugin_websocket_boundary_v1",
        "acceptedThenClosed": true,
        "accepted_then_closed": true,
        "closeCode": 1000,
        "close_code": 1000,
        "nativeKanbanEventsBridge": true,
        "native_kanban_events_bridge": true,
        "queryTokenAuthCompatible": true,
        "query_token_auth_compatible": true,
        "websocketHandlerExecution": false,
        "websocket_handler_execution": false,
        "arbitraryLongLivedPythonHandler": false,
        "arbitrary_long_lived_python_handler": false,
        "fastApiDependencyLifecycle": false,
        "fastapi_dependency_lifecycle": false,
        "externalFastApiHostPlan": {
            "command": "hermes dashboard --no-open",
            "defaultUrl": "http://127.0.0.1:9119",
            "routesMountedByHermes": "/api/plugins/{plugin}/{path}",
            "webSocketExample": "/api/plugins/kanban/events?since=0&token=<session-token>"
        },
        "external_fastapi_host_plan": {
            "command": "hermes dashboard --no-open",
            "default_url": "http://127.0.0.1:9119",
            "routes_mounted_by_hermes": "/api/plugins/{plugin}/{path}",
            "websocket_example": "/api/plugins/kanban/events?since=0&token=<session-token>"
        },
        "safeSmokeTests": [
            "Use dashboard_plugins status to confirm @router.websocket declarations are discovered.",
            "Open SynthChat native /api/plugins/kanban/events for the supported Kanban event bridge.",
            "For arbitrary third-party WebSocket handlers, start the external Hermes FastAPI host with hermes dashboard --no-open and connect to /api/plugins/{plugin}/{path} there."
        ],
        "safe_smoke_tests": [
            "Use dashboard_plugins status to confirm @router.websocket declarations are discovered.",
            "Open SynthChat native /api/plugins/kanban/events for the supported Kanban event bridge.",
            "For arbitrary third-party WebSocket handlers, start the external Hermes FastAPI host with hermes dashboard --no-open and connect to /api/plugins/{plugin}/{path} there."
        ],
        "boundary": "SynthChat discovers trusted dashboard plugin WebSocket declarations and the native API server can accept a declared upgrade, emit hermes_dashboard_plugin_websocket_boundary_v1, then close cleanly. Native Kanban events have a real WebSocket bridge. Arbitrary long-lived Python WebSocket handlers still require the external Hermes FastAPI dashboard host."
    })
}

fn hermes_dashboard_plugins(store: &AppStore) -> Vec<Value> {
    let mut plugins = vec![
        json!({
            "name": "hermes-achievements",
            "label": "Achievements",
            "description": "Steam-style achievements for vibe coding and agentic Hermes workflows.",
            "icon": "Star",
            "version": "0.4.0",
            "tab": {"path": "/achievements", "position": "after:analytics"},
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "api": "plugin_api.py",
            "manifestPath": "plugins/hermes-achievements/dashboard/manifest.json",
            "bundleKind": "dashboard_plugin",
            "stateRoot": hermes_home(store).join("plugins").join("hermes-achievements").to_string_lossy().to_string(),
            "modelToolsRegistered": false
        }),
        json!({
            "name": "example",
            "label": "Example",
            "description": "Example dashboard plugin - used by test suite for auth coverage",
            "icon": "Sparkles",
            "version": "1.0.0",
            "tab": {"path": "/example", "position": "after:skills"},
            "slots": [],
            "entry": "dist/index.js",
            "api": "plugin_api.py",
            "manifestPath": "plugins/example-dashboard/dashboard/manifest.json",
            "bundleKind": "dashboard_plugin",
            "modelToolsRegistered": false
        }),
        json!({
            "name": "kanban",
            "label": "Kanban",
            "description": "Multi-agent collaboration board - drag-drop cards across columns, comment threads, worker runs, and profile orchestration.",
            "icon": "Package",
            "version": "1.0.0",
            "tab": {"path": "/kanban", "position": "after:skills"},
            "entry": "dist/index.js",
            "css": "dist/style.css",
            "api": "plugin_api.py",
            "manifestPath": "plugins/kanban/dashboard/manifest.json",
            "bundleKind": "dashboard_plugin",
            "modelToolsRegistered": false,
            "nativeAgentToolsAdapted": true
        }),
    ];
    let mut names = plugins
        .iter()
        .filter_map(|plugin| plugin.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    for plugin in hermes_user_dashboard_plugins(store) {
        let Some(name) = plugin.get("name").and_then(Value::as_str) else {
            continue;
        };
        if names.insert(name.to_string()) {
            plugins.push(plugin);
        }
    }
    plugins
}

fn hermes_user_dashboard_plugins(store: &AppStore) -> Vec<Value> {
    let root = hermes_home(store).join("plugins");
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        let dashboard_dir = plugin_dir.join("dashboard");
        let manifest_path = dashboard_dir.join("manifest.json");
        let Some(mut manifest) = read_json_file(&manifest_path) else {
            continue;
        };
        let fallback_name = plugin_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("user-plugin")
            .to_string();
        if !manifest.is_object() {
            manifest = json!({});
        }
        if let Some(object) = manifest.as_object_mut() {
            object
                .entry("name")
                .or_insert_with(|| json!(fallback_name.clone()));
            object
                .entry("label")
                .or_insert_with(|| json!(fallback_name.clone()));
            object
                .entry("api")
                .or_insert_with(|| json!("plugin_api.py"));
            object
                .entry("bundleKind")
                .or_insert_with(|| json!("dashboard_plugin"));
            object.entry("source").or_insert_with(|| json!("user"));
            object.insert(
                "manifestPath".into(),
                json!(manifest_path.to_string_lossy().to_string()),
            );
            object
                .entry("modelToolsRegistered")
                .or_insert_with(|| json!(false));
        }
        plugins.push(manifest);
    }
    plugins
}

fn hermes_home(store: &AppStore) -> PathBuf {
    env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| store.data_dir().join(".hermes"))
}

fn read_json_file(path: &PathBuf) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

const KANBAN_DASHBOARD_COLUMNS: &[&str] = &[
    "triage",
    "todo",
    "scheduled",
    "ready",
    "running",
    "blocked",
    "review",
    "done",
];

fn kanban_dashboard_board(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let include_archived = payload
        .get("includeArchived")
        .or_else(|| payload.get("include_archived"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tenant = payload.get("tenant").and_then(Value::as_str);
    let tasks = filtered_kanban_tasks(store, include_archived, tenant)?;
    let mut columns = KANBAN_DASHBOARD_COLUMNS
        .iter()
        .map(|name| ((*name).to_string(), Vec::<Value>::new()))
        .collect::<BTreeMap<_, _>>();
    if include_archived {
        columns.entry("archived".into()).or_default();
    }
    let mut tenants = BTreeSet::new();
    let mut assignees = BTreeSet::new();
    let mut latest_event_id = 0usize;
    for task in tasks {
        if let Some(tenant) = task.get("tenant").and_then(Value::as_str) {
            if !tenant.trim().is_empty() {
                tenants.insert(tenant.to_string());
            }
        }
        if let Some(assignee) = task.get("assignee").and_then(Value::as_str) {
            if !assignee.trim().is_empty() {
                assignees.insert(assignee.to_string());
            }
        }
        latest_event_id += task
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let column = kanban_dashboard_column_for_task(&task);
        columns
            .entry(column)
            .or_insert_with(Vec::new)
            .push(kanban_dashboard_task_card(&task));
    }
    let column_values = columns
        .into_iter()
        .map(|(name, tasks)| json!({"name": name, "tasks": tasks}))
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-board",
        "columns": column_values,
        "tenants": tenants,
        "assignees": assignees,
        "latest_event_id": latest_event_id,
        "now": Utc::now().timestamp(),
        "source": "SynthChat AppStore.agent_kanban_tasks",
        "board": payload.get("board").and_then(Value::as_str).unwrap_or("default"),
        "nativeDashboardRead": true
    }))
}

fn kanban_dashboard_config(store: &AppStore) -> AppResult<Value> {
    let config = store.config()?;
    let dashboard = config
        .messaging_gateway
        .get("dashboard")
        .and_then(Value::as_object);
    let dashboard_plugins = config
        .messaging_gateway
        .get("dashboardPlugins")
        .or_else(|| config.messaging_gateway.get("dashboard_plugins"));
    let kanban = dashboard
        .and_then(|value| value.get("kanban"))
        .or_else(|| dashboard_plugins.and_then(|value| value.get("kanbanConfig")))
        .or_else(|| dashboard_plugins.and_then(|value| value.get("kanban_config")))
        .or_else(|| config.messaging_gateway.get("dashboardKanban"))
        .or_else(|| config.messaging_gateway.get("dashboard_kanban"))
        .filter(|value| value.is_object());
    Ok(json!({
        "schema": "hermes_kanban_config_desktop_v1",
        "status": "ok",
        "action": "kanban-config",
        "default_tenant": kanban
            .and_then(|value| value.get("default_tenant").or_else(|| value.get("defaultTenant")))
            .and_then(Value::as_str)
            .unwrap_or(""),
        "lane_by_profile": kanban_bool(kanban, &["lane_by_profile", "laneByProfile"], true),
        "include_archived_by_default": kanban_bool(
            kanban,
            &["include_archived_by_default", "includeArchivedByDefault"],
            false,
        ),
        "render_markdown": kanban_bool(kanban, &["render_markdown", "renderMarkdown"], true),
        "source": "SynthChat config.messaging_gateway.dashboard.kanban",
        "nativeDashboardRead": true
    }))
}

fn kanban_bool(config: Option<&Value>, keys: &[&str], default: bool) -> bool {
    keys.iter()
        .find_map(|key| {
            config
                .and_then(|value| value.get(*key))
                .and_then(Value::as_bool)
        })
        .unwrap_or(default)
}

fn kanban_dashboard_stats(store: &AppStore) -> AppResult<Value> {
    let tasks = store.agent_kanban_tasks()?;
    let mut per_status = BTreeMap::<String, usize>::new();
    let mut per_assignee = BTreeMap::<String, usize>::new();
    let mut oldest_ready_age_seconds: Option<i64> = None;
    let now = Utc::now();
    for task in &tasks {
        let status = kanban_dashboard_status(task);
        *per_status.entry(status.clone()).or_default() += 1;
        if let Some(assignee) = task.get("assignee").and_then(Value::as_str) {
            if !assignee.trim().is_empty() {
                *per_assignee.entry(assignee.to_string()).or_default() += 1;
            }
        }
        if status == "ready" {
            if let Some(created_at) = task
                .get("createdAt")
                .or_else(|| task.get("created_at"))
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
            {
                let age = (now - created_at).num_seconds().max(0);
                oldest_ready_age_seconds =
                    Some(oldest_ready_age_seconds.map_or(age, |old| old.max(age)));
            }
        }
    }
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-stats",
        "total": tasks.len(),
        "per_status": per_status,
        "per_assignee": per_assignee,
        "oldest_ready_age_seconds": oldest_ready_age_seconds,
        "source": "SynthChat AppStore.agent_kanban_tasks",
        "nativeDashboardRead": true
    }))
}

fn kanban_dashboard_assignees(store: &AppStore) -> AppResult<Value> {
    let tasks = store.agent_kanban_tasks()?;
    let mut counts = BTreeMap::<String, usize>::new();
    for task in &tasks {
        if let Some(assignee) = task.get("assignee").and_then(Value::as_str) {
            if !assignee.trim().is_empty() {
                *counts.entry(assignee.to_string()).or_default() += 1;
            }
        }
    }
    let assignees = counts
        .iter()
        .map(|(name, count)| {
            json!({
                "name": name,
                "label": name,
                "task_count": count,
                "source": "task_assignee"
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-assignees",
        "assignees": assignees,
        "count": assignees.len(),
        "source": "SynthChat AppStore.agent_kanban_tasks",
        "nativeDashboardRead": true
    }))
}

fn kanban_dashboard_task(store: &AppStore, task_id: &str) -> AppResult<Value> {
    let task = store
        .agent_kanban_tasks()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id));
    let Some(task) = task else {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "not_found",
            "action": "kanban-task",
            "taskId": task_id,
            "task": Value::Null,
            "comments": [],
            "events": [],
            "attachments": [],
            "links": {"parents": [], "children": []},
            "runs": []
        }));
    };
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-task",
        "task": kanban_dashboard_task_card(&task),
        "comments": task.get("comments").cloned().unwrap_or_else(|| json!([])),
        "events": task.get("events").cloned().unwrap_or_else(|| json!([])),
        "attachments": task.get("attachments").cloned().unwrap_or_else(|| json!([])),
        "links": {
            "parents": task.get("parents").cloned().unwrap_or_else(|| json!([])),
            "children": task.get("children").cloned().unwrap_or_else(|| json!([]))
        },
        "runs": task.get("runs").cloned().unwrap_or_else(|| json!([])),
        "source": "SynthChat AppStore.agent_kanban_tasks",
        "nativeDashboardRead": true
    }))
}

fn kanban_dashboard_events(store: &AppStore, payload: &Value, action: &str) -> AppResult<Value> {
    let since = payload
        .get("since")
        .or_else(|| payload.get("cursor"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        })
        .unwrap_or(0);
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 200) as usize;
    let board = payload
        .get("board")
        .and_then(Value::as_str)
        .map(normalize_kanban_board_slug)
        .filter(|value| !value.is_empty());
    let mut flattened = Vec::<(String, usize, String, Value)>::new();
    for (task_index, task) in store.agent_kanban_tasks()?.into_iter().enumerate() {
        if board
            .as_deref()
            .is_some_and(|board| kanban_board_for_task(&task) != board)
        {
            continue;
        }
        let task_id = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (event_index, event) in task
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let created_at = event
                .get("createdAt")
                .or_else(|| event.get("created_at"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let stable_order = task_index
                .saturating_mul(10_000)
                .saturating_add(event_index);
            flattened.push((created_at, stable_order, task_id.clone(), event.clone()));
        }
    }
    flattened.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

    let total = flattened.len() as u64;
    let mut cursor = since.min(total);
    let events = flattened
        .into_iter()
        .enumerate()
        .filter_map(|(index, (created_at, _stable_order, task_id, event))| {
            let id = index as u64 + 1;
            if id <= since {
                return None;
            }
            cursor = id;
            Some(json!({
                "id": id,
                "task_id": task_id,
                "run_id": event.get("runId").or_else(|| event.get("run_id")).cloned().unwrap_or(Value::Null),
                "kind": event.get("kind").cloned().unwrap_or_else(|| json!("event")),
                "payload": event.get("payload").cloned().unwrap_or(Value::Null),
                "created_at": if created_at.is_empty() { Value::Null } else { json!(created_at) },
                "source": "task.events"
            }))
        })
        .take(limit)
        .collect::<Vec<_>>();
    if let Some(last) = events
        .last()
        .and_then(|event| event.get("id"))
        .and_then(Value::as_u64)
    {
        cursor = last;
    }

    let count = events.len();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": action,
        "events": events,
        "cursor": cursor,
        "count": count,
        "total": total,
        "since": since,
        "limit": limit,
        "board": board,
        "pollIntervalMs": 300,
        "websocketEmbedded": true,
        "nativeApiServerWebSocket": true,
        "nativeEventStream": true,
        "source": "SynthChat AppStore.agent_kanban_tasks events",
        "boundary": "Hermes exposes this as WebSocket /api/plugins/kanban/events?since=. SynthChat exposes the same cursor payload through dashboard_plugins polling and a native API-server WebSocket bridge without embedding the full FastAPI dashboard host."
    }))
}

pub(super) fn kanban_dashboard_runtime_events(
    store: &AppStore,
    payload: &Value,
    action: &str,
) -> AppResult<Value> {
    let since = payload
        .get("since")
        .or_else(|| payload.get("cursor"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        })
        .unwrap_or(0);
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 200) as usize;
    let board = payload
        .get("board")
        .and_then(Value::as_str)
        .map(normalize_kanban_board_slug)
        .filter(|value| !value.is_empty());
    let conversation_id = payload
        .get("conversationId")
        .or_else(|| payload.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let run_id_filter = payload
        .get("runId")
        .or_else(|| payload.get("run_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let queue_item_filter = payload
        .get("queueItemId")
        .or_else(|| payload.get("queue_item_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let task_id_filter = payload
        .get("taskId")
        .or_else(|| payload.get("task_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut flattened = Vec::<(String, usize, Value)>::new();
    let mut order = 0usize;
    for item in store.agent_queue()? {
        if conversation_id
            .as_deref()
            .is_some_and(|filter| item.conversation_id != filter)
            || queue_item_filter
                .as_deref()
                .is_some_and(|filter| item.id != filter)
        {
            continue;
        }
        let created_at = item.updated_at.clone();
        let kind = match item.status.as_str() {
            "running" => "queue_running",
            "completed" => "queue_completed",
            "failed" => "queue_failed",
            "canceled" => "queue_canceled",
            _ => "queue_pending",
        };
        flattened.push((
            created_at.clone(),
            next_runtime_event_order(&mut order),
            json!({
                "kind": kind,
                "queue_item_id": item.id,
                "conversation_id": item.conversation_id,
                "run_id": Value::Null,
                "status": item.status,
                "payload": {
                    "personaId": item.persona_id,
                    "userMessageId": item.user_message_id,
                    "content": item.content,
                    "createdAt": item.created_at,
                    "updatedAt": item.updated_at,
                    "startedAt": item.started_at,
                    "completedAt": item.completed_at,
                    "error": item.error
                },
                "created_at": created_at,
                "source": "agent_queue"
            }),
        ));
    }

    for run in store.agent_runs()? {
        if conversation_id
            .as_deref()
            .is_some_and(|filter| run.conversation_id != filter)
            || run_id_filter
                .as_deref()
                .is_some_and(|filter| run.run_id != filter)
            || queue_item_filter
                .as_deref()
                .is_some_and(|filter| run.queue_item_id.as_deref() != Some(filter))
        {
            continue;
        }
        flattened.push((
            run.updated_at.clone(),
            next_runtime_event_order(&mut order),
            json!({
                "kind": format!("run_{}", run.state),
                "run_id": run.run_id,
                "queue_item_id": run.queue_item_id,
                "conversation_id": run.conversation_id,
                "status": run.state,
                "payload": {
                    "agentId": run.agent_id,
                    "personaId": run.persona_id,
                    "parentRunId": run.parent_run_id,
                    "lastActivityAt": run.last_activity_at,
                    "lastActivityDesc": run.last_activity_desc,
                    "completedAt": run.completed_at,
                    "error": run.error
                },
                "created_at": run.updated_at,
                "source": "agent_runs"
            }),
        ));
        for phase in run.phase_events {
            flattened.push((
                phase.updated_at.clone(),
                next_runtime_event_order(&mut order),
                json!({
                    "kind": format!("run_phase_{}", phase.phase),
                    "run_id": run.run_id,
                    "queue_item_id": run.queue_item_id,
                    "conversation_id": run.conversation_id,
                    "status": run.state,
                    "payload": {
                        "phase": phase.phase,
                        "detail": phase.detail
                    },
                    "created_at": phase.updated_at,
                    "source": "agent_run.phase_events"
                }),
            ));
        }
        for tool_event in run.tool_events {
            let status = tool_event
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            flattened.push((
                tool_event
                    .get("createdAt")
                    .or_else(|| tool_event.get("created_at"))
                    .and_then(Value::as_str)
                    .unwrap_or(run.updated_at.as_str())
                    .to_string(),
                next_runtime_event_order(&mut order),
                json!({
                    "kind": format!("tool_{status}"),
                    "run_id": run.run_id,
                    "queue_item_id": run.queue_item_id,
                    "conversation_id": run.conversation_id,
                    "status": status,
                    "payload": tool_event,
                    "created_at": tool_event
                        .get("createdAt")
                        .or_else(|| tool_event.get("created_at"))
                        .and_then(Value::as_str)
                        .unwrap_or(run.updated_at.as_str()),
                    "source": "agent_run.tool_events"
                }),
            ));
        }
    }

    for process in store.managed_processes()? {
        let process_run_id = process
            .get("runId")
            .or_else(|| process.get("run_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let process_conversation_id = process
            .get("conversationId")
            .or_else(|| process.get("conversation_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if conversation_id
            .as_deref()
            .is_some_and(|filter| process_conversation_id != filter)
            || run_id_filter
                .as_deref()
                .is_some_and(|filter| process_run_id != filter)
        {
            continue;
        }
        let status = process
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let created_at = process
            .get("updatedAt")
            .or_else(|| process.get("updated_at"))
            .or_else(|| process.get("startedAt"))
            .or_else(|| process.get("started_at"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        flattened.push((
            created_at.clone(),
            next_runtime_event_order(&mut order),
            json!({
                "kind": format!("process_{status}"),
                "run_id": if process_run_id.is_empty() { Value::Null } else { json!(process_run_id) },
                "queue_item_id": Value::Null,
                "conversation_id": if process_conversation_id.is_empty() { Value::Null } else { json!(process_conversation_id) },
                "status": status,
                "payload": process,
                "created_at": if created_at.is_empty() { Value::Null } else { json!(created_at) },
                "source": "managed_processes"
            }),
        ));
    }

    for (task_index, task) in store.agent_kanban_tasks()?.into_iter().enumerate() {
        if board
            .as_deref()
            .is_some_and(|board| kanban_board_for_task(&task) != board)
            || task_id_filter
                .as_deref()
                .is_some_and(|filter| task.get("id").and_then(Value::as_str) != Some(filter))
        {
            continue;
        }
        let task_id = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (event_index, event) in task
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let created_at = event
                .get("createdAt")
                .or_else(|| event.get("created_at"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let stable_order = task_index
                .saturating_mul(10_000)
                .saturating_add(event_index);
            flattened.push((
                created_at.clone(),
                next_runtime_event_order(&mut order).saturating_add(stable_order),
                json!({
                    "kind": event.get("kind").cloned().unwrap_or_else(|| json!("kanban_event")),
                    "task_id": task_id,
                    "run_id": event.get("runId").or_else(|| event.get("run_id")).cloned().unwrap_or(Value::Null),
                    "queue_item_id": event
                        .get("payload")
                        .and_then(|payload| payload.get("queueItemId").or_else(|| payload.get("queue_item_id")))
                        .cloned()
                        .unwrap_or(Value::Null),
                    "status": event.get("kind").cloned().unwrap_or_else(|| json!("event")),
                    "payload": event.get("payload").cloned().unwrap_or(Value::Null),
                    "created_at": if created_at.is_empty() { Value::Null } else { json!(created_at) },
                    "source": "task.events"
                }),
            ));
        }
    }

    flattened.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let total = flattened.len() as u64;
    let mut cursor = since.min(total);
    let events = flattened
        .into_iter()
        .enumerate()
        .filter_map(|(index, (_created_at, _stable_order, mut event))| {
            let id = index as u64 + 1;
            if id <= since {
                return None;
            }
            cursor = id;
            if let Some(object) = event.as_object_mut() {
                object.insert("id".into(), json!(id));
            }
            Some(event)
        })
        .take(limit)
        .collect::<Vec<_>>();
    if let Some(last) = events
        .last()
        .and_then(|event| event.get("id"))
        .and_then(Value::as_u64)
    {
        cursor = last;
    }
    let count = events.len();
    Ok(json!({
        "schema": "hermes_kanban_runtime_events_desktop_v1",
        "status": "ok",
        "action": action,
        "events": events,
        "cursor": cursor,
        "count": count,
        "total": total,
        "since": since,
        "limit": limit,
        "board": board,
        "conversationId": conversation_id,
        "runId": run_id_filter,
        "queueItemId": queue_item_filter,
        "taskId": task_id_filter,
        "pollIntervalMs": 300,
        "websocketEmbedded": false,
        "nativeRuntimeEventBridge": true,
        "sources": ["agent_queue", "agent_runs", "agent_run.phase_events", "agent_run.tool_events", "managed_processes", "task.events"],
        "boundary": "Hermes dashboard streams Kanban, worker, and tool transitions over a hosted websocket. SynthChat now exposes the same transition model as a desktop-native cursor stream by merging queue items, AgentRun phase/tool events, ManagedProcess snapshots, and Kanban task events, with native API bridges for Kanban events and runtime-events SSE. The remaining boundary is the full embedded dashboard host for arbitrary plugin websocket handlers."
    }))
}

fn next_runtime_event_order(order: &mut usize) -> usize {
    let current = *order;
    *order = order.saturating_add(1);
    current
}

fn kanban_dashboard_write_response(action: &str, result: String) -> AppResult<Value> {
    let mut value = serde_json::from_str::<Value>(&result)?;
    if !value.is_object() {
        value = json!({"result": value});
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "schema".into(),
            Value::String("hermes_kanban_dashboard_desktop_v1".into()),
        );
        object.insert("status".into(), Value::String("ok".into()));
        object.insert("action".into(), Value::String(action.into()));
        object.insert(
            "source".into(),
            Value::String("SynthChat AppStore.agent_kanban_tasks".into()),
        );
        object.insert("nativeDashboardWrite".into(), Value::Bool(true));
        if let Some(task) = object.get("task").cloned() {
            object.insert("taskCard".into(), kanban_dashboard_task_card(&task));
        }
        if let Some(updated) = object.get("updated").and_then(Value::as_array).cloned() {
            object.insert(
                "taskCards".into(),
                Value::Array(
                    updated
                        .iter()
                        .map(kanban_dashboard_task_card)
                        .collect::<Vec<_>>(),
                ),
            );
        }
    }
    Ok(value)
}

fn kanban_dashboard_specify(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let task_id = payload_string(payload, &["taskId", "task_id", "id"])
        .ok_or_else(|| AppError::BadRequest("kanban-specify requires taskId".into()))?;
    let mut tool_payload = payload.clone();
    tool_payload["taskId"] = json!(task_id.clone());
    let outcome = tauri::async_runtime::block_on(kanban_specify_tool(store, &tool_payload));
    match outcome {
        Ok(result) => {
            let value = kanban_dashboard_write_response("kanban-specify", result)?;
            let new_title = value
                .get("task")
                .and_then(|task| task.get("title"))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(json!({
                "schema": "hermes_kanban_dashboard_desktop_v1",
                "status": "ok",
                "action": "kanban-specify",
                "ok": true,
                "task_id": task_id,
                "taskId": task_id,
                "reason": value.get("reason").cloned().unwrap_or(Value::Null),
                "new_title": new_title,
                "newTitle": new_title,
                "task": value.get("task").cloned().unwrap_or(Value::Null),
                "taskCard": value.get("taskCard").cloned().unwrap_or(Value::Null),
                "source": value.get("source").cloned().unwrap_or_else(|| json!("SynthChat AppStore.agent_kanban_tasks")),
                "model": value.get("model").cloned().unwrap_or(Value::Null),
                "nativeDashboardWrite": true
            }))
        }
        Err(error) => Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "failed",
            "action": "kanban-specify",
            "ok": false,
            "task_id": task_id,
            "taskId": task_id,
            "reason": error.to_string(),
            "new_title": Value::Null,
            "newTitle": Value::Null,
            "nativeDashboardWrite": true
        })),
    }
}

fn kanban_dashboard_decompose(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let task_id = payload_string(payload, &["taskId", "task_id", "id"])
        .ok_or_else(|| AppError::BadRequest("kanban-decompose requires taskId".into()))?;
    let task = store
        .agent_kanban_tasks()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
    let objective = payload_string(payload, &["objective", "task", "prompt"])
        .or_else(|| {
            task.get("body")
                .or_else(|| task.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            task.get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| task_id.clone());
    let mut tool_payload = payload.clone();
    tool_payload["taskId"] = json!(task_id.clone());
    tool_payload["objective"] = json!(objective);
    tool_payload["create"] = payload
        .get("create")
        .or_else(|| payload.get("createTasks"))
        .or_else(|| payload.get("create_tasks"))
        .cloned()
        .unwrap_or_else(|| json!(true));
    if tool_payload.get("parents").is_none()
        && tool_payload.get("parentIds").is_none()
        && tool_payload.get("parent_ids").is_none()
    {
        tool_payload["parents"] = json!([task_id.clone()]);
    }
    let outcome = tauri::async_runtime::block_on(kanban_decompose_tool(store, &tool_payload));
    match outcome {
        Ok(result) => {
            let mut value = kanban_dashboard_write_response("kanban-decompose", result)?;
            let created_tasks = value
                .get("createdTasks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let child_ids = created_tasks
                .iter()
                .filter_map(|task| task.get("id").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>();
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".into(), json!(true));
                object.insert("task_id".into(), json!(task_id.clone()));
                object.insert("taskId".into(), json!(task_id.clone()));
                object.insert("fanout".into(), json!(!child_ids.is_empty()));
                object.insert("child_ids".into(), json!(child_ids));
                object.insert("new_title".into(), Value::Null);
                object.insert("newTitle".into(), Value::Null);
                object.insert("nativeDashboardWrite".into(), json!(true));
            }
            Ok(value)
        }
        Err(error) => Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "failed",
            "action": "kanban-decompose",
            "ok": false,
            "task_id": task_id,
            "taskId": task_id,
            "reason": error.to_string(),
            "fanout": false,
            "child_ids": [],
            "new_title": Value::Null,
            "newTitle": Value::Null,
            "nativeDashboardWrite": true
        })),
    }
}

fn kanban_dashboard_home_channels(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let task_id = payload_string(payload, &["taskId", "task_id", "id"]);
    let subscribed = task_id
        .as_deref()
        .and_then(|id| kanban_task_notify_subs(store, id).ok())
        .unwrap_or_default();
    let home_channels = configured_kanban_home_channels(store)?
        .into_iter()
        .map(|home| {
            let subscribed = subscribed
                .iter()
                .any(|sub| kanban_home_sub_matches(sub, &home));
            json!({
                "platform": home.platform,
                "chat_id": home.chat_id,
                "thread_id": home.thread_id.unwrap_or_default(),
                "name": home.name,
                "subscribed": subscribed
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-home-channels",
        "task_id": task_id,
        "taskId": task_id,
        "home_channels": home_channels,
        "nativeHomeChannelBridge": true,
        "boundary": "SynthChat reads desktop platform homeChannel settings and stores per-task notify subscriptions in task metadata; actual delivery is delegated to configured messaging adapters."
    }))
}

fn kanban_dashboard_home_subscription(
    store: &AppStore,
    payload: &Value,
    subscribe: bool,
) -> AppResult<Value> {
    let task_id = payload_string(payload, &["taskId", "task_id", "id"])
        .ok_or_else(|| AppError::BadRequest("kanban home subscription requires taskId".into()))?;
    let platform = payload_string(payload, &["platform"])
        .ok_or_else(|| AppError::BadRequest("kanban home subscription requires platform".into()))?;
    let home = configured_kanban_home_channels(store)?
        .into_iter()
        .find(|home| home.platform == platform)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No home channel configured for platform {platform:?}"
            ))
        })?;
    let mut tasks = store.agent_kanban_tasks()?;
    let task = tasks
        .iter_mut()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
    let mut metadata = task
        .get("metadata")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut subs = metadata
        .get("notifySubs")
        .or_else(|| metadata.get("notify_subs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let home_value = json!({
        "platform": home.platform,
        "chat_id": home.chat_id,
        "chatId": home.chat_id,
        "thread_id": home.thread_id,
        "threadId": home.thread_id,
        "name": home.name,
        "notifier_profile": active_kanban_profile_name(store),
        "notifierProfile": active_kanban_profile_name(store),
        "source": "dashboard_plugins.kanban-home-subscribe"
    });
    let before = subs.len();
    if subscribe {
        if !subs.iter().any(|sub| kanban_home_sub_matches(sub, &home)) {
            subs.push(home_value.clone());
        }
    } else {
        subs.retain(|sub| !kanban_home_sub_matches(sub, &home));
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("notifySubs".into(), Value::Array(subs));
    }
    task["metadata"] = metadata;
    task["updatedAt"] = json!(now_iso());
    kanban_push_dashboard_event(
        task,
        if subscribe {
            "home_subscribed"
        } else {
            "home_unsubscribed"
        },
        json!({
            "platform": platform,
            "home_channel": home_value,
            "changed": before != task.get("metadata").and_then(|m| m.get("notifySubs")).and_then(Value::as_array).map(Vec::len).unwrap_or(before),
            "source": "dashboard_plugins.kanban-home-subscription"
        }),
    );
    let task_card = kanban_dashboard_task_card(task);
    store.set_agent_kanban_tasks(tasks)?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": if subscribe { "kanban-home-subscribe" } else { "kanban-home-unsubscribe" },
        "ok": true,
        "task_id": task_id,
        "taskId": task_id,
        "home_channel": home_value,
        "task": task_card,
        "nativeHomeChannelBridge": true
    }))
}

fn kanban_dashboard_diagnostics(store: &AppStore, severity: Option<&str>) -> AppResult<Value> {
    let mut diagnostics = Vec::new();
    for task in store.agent_kanban_tasks()? {
        let task_id = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let task_status = kanban_dashboard_status(&task);
        let mut items = Vec::new();
        if task_status == "blocked" {
            items.push(json!({
                "severity": "warning",
                "kind": "blocked_task",
                "message": task.get("blockReason").or_else(|| task.get("block_reason")).and_then(Value::as_str).unwrap_or("Task is blocked"),
                "last_seen_at": Utc::now().timestamp()
            }));
        }
        if task_status == "running" {
            let has_worker = kanban_managed_processes_for_task(store, &task_id)?
                .iter()
                .any(|process| process["status"] == "running");
            if !has_worker {
                items.push(json!({
                    "severity": "error",
                    "kind": "running_without_worker",
                    "message": "Task is running but no SynthChat managed process is active for this task",
                    "last_seen_at": Utc::now().timestamp()
                }));
            }
        }
        if let Some(filter) = severity {
            items.retain(|item| kanban_severity_matches(item["severity"].as_str(), filter));
        }
        if !items.is_empty() {
            diagnostics.push(json!({
                "task_id": task_id,
                "task_title": task.get("title").cloned().unwrap_or(Value::Null),
                "task_status": task_status,
                "task_assignee": task.get("assignee").cloned().unwrap_or(Value::Null),
                "diagnostics": items
            }));
        }
    }
    let count = diagnostics
        .iter()
        .map(|item| {
            item.get("diagnostics")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        })
        .sum::<usize>();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-diagnostics",
        "diagnostics": diagnostics,
        "count": count,
        "source": "SynthChat AppStore.agent_kanban_tasks + ManagedProcess registry",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_dashboard_workers_active(store: &AppStore) -> AppResult<Value> {
    let workers = store
        .managed_processes()?
        .into_iter()
        .filter(|process| {
            process.get("taskId").and_then(Value::as_str).is_some()
                && process.get("status").and_then(Value::as_str) == Some("running")
        })
        .map(|process| {
            json!({
                "run_id": process.get("runId").cloned().unwrap_or(Value::Null),
                "task_id": process.get("taskId").cloned().unwrap_or(Value::Null),
                "task_title": kanban_task_title_for_id(store, process.get("taskId").and_then(Value::as_str)).unwrap_or(Value::Null),
                "task_status": kanban_task_status_for_id(store, process.get("taskId").and_then(Value::as_str)).unwrap_or(Value::Null),
                "task_assignee": kanban_task_assignee_for_id(store, process.get("taskId").and_then(Value::as_str)).unwrap_or(Value::Null),
                "profile": process.get("label").cloned().unwrap_or(Value::Null),
                "worker_pid": process.get("pid").cloned().unwrap_or(Value::Null),
                "started_at": process.get("startedAt").cloned().unwrap_or(Value::Null),
                "claim_lock": process.get("id").cloned().unwrap_or(Value::Null),
                "claim_expires": Value::Null,
                "last_heartbeat_at": Value::Null,
                "max_runtime_seconds": Value::Null,
                "managed_process": process
            })
        })
        .collect::<Vec<_>>();
    let worker_count = workers.len();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-workers-active",
        "workers": workers,
        "count": worker_count,
        "checked_at": Utc::now().timestamp(),
        "source": "SynthChat ManagedProcess registry",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_dashboard_run(store: &AppStore, run_id: &str) -> AppResult<Value> {
    if run_id.trim().is_empty() {
        return Ok(kanban_not_found_payload("kanban-run", "run", run_id));
    }
    if let Some(process) = kanban_managed_process_by_run(store, run_id)? {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "ok",
            "action": "kanban-run",
            "run": kanban_run_from_process(&process),
            "managed_process": process,
            "source": "SynthChat ManagedProcess registry",
            "nativeWorkerVisibility": true
        }));
    }
    if let Ok(run) = store.agent_run(run_id) {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "ok",
            "action": "kanban-run",
            "run": {
                "run_id": run.run_id,
                "task_id": Value::Null,
                "status": run.state,
                "profile": run.persona_id,
                "started_at": run.started_at,
                "ended_at": run.completed_at,
                "summary": run.last_activity_desc.or(run.error),
                "worker_pid": Value::Null
            },
            "source": "SynthChat agent_runs",
            "nativeWorkerVisibility": true
        }));
    }
    Ok(kanban_not_found_payload("kanban-run", "run", run_id))
}

fn kanban_dashboard_run_inspect(store: &AppStore, run_id: &str) -> AppResult<Value> {
    if let Some(process) = kanban_managed_process_by_run(store, run_id)? {
        let alive = process.get("status").and_then(Value::as_str) == Some("running");
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "ok",
            "action": "kanban-run-inspect",
            "run_id": run_id,
            "alive": alive,
            "pid": process.get("pid").cloned().unwrap_or(Value::Null),
            "reason": if alive { Value::Null } else { json!("managed process is not running") },
            "managed_process": process,
            "source": "SynthChat ManagedProcess registry",
            "nativeWorkerVisibility": true
        }));
    }
    Ok(kanban_not_found_payload(
        "kanban-run-inspect",
        "run",
        run_id,
    ))
}

fn kanban_dashboard_run_terminate(
    store: &AppStore,
    run_id: &str,
    reason: Option<&str>,
) -> AppResult<Value> {
    let stopped = store.stop_managed_processes(None, None, Some(run_id), None, None, true)?;
    let stopped_count = stopped.get("count").and_then(Value::as_u64).unwrap_or(0);
    let aborted = if stopped_count == 0 {
        store
            .abort_agent_run(
                run_id,
                Some(reason.unwrap_or("Kanban dashboard terminate").into()),
            )
            .ok()
            .map(|run| {
                json!({
                    "run_id": run.run_id,
                    "status": run.state,
                    "summary": run.last_activity_desc.or(run.error)
                })
            })
    } else {
        None
    };
    let ok = stopped_count > 0 || aborted.is_some();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": if ok { "ok" } else { "not_found" },
        "action": "kanban-run-terminate",
        "ok": ok,
        "run_id": run_id,
        "stopped": stopped,
        "aborted_agent_run": aborted,
        "source": "SynthChat ManagedProcess registry + agent_runs",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_dashboard_reclaim(
    store: &AppStore,
    task_id: &str,
    reason: Option<&str>,
) -> AppResult<Value> {
    if task_id.trim().is_empty() {
        return Ok(kanban_not_found_payload("kanban-reclaim", "task", task_id));
    }

    let mut tasks = store.agent_kanban_tasks()?;
    let Some(index) = tasks
        .iter()
        .position(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
    else {
        return Ok(kanban_not_found_payload("kanban-reclaim", "task", task_id));
    };

    let task = &mut tasks[index];
    let status = kanban_dashboard_status(task);
    let claim_lock = task
        .get("claimLock")
        .or_else(|| task.get("claim_lock"))
        .cloned()
        .unwrap_or(Value::Null);
    let current_run_id = task
        .get("currentRunId")
        .or_else(|| task.get("current_run_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            task.get("runs")
                .and_then(Value::as_array)
                .and_then(|runs| runs.iter().rev().find(|run| run["status"] == "running"))
                .and_then(|run| run.get("run_id").and_then(Value::as_str))
                .map(str::to_string)
        });
    if status != "running" && claim_lock.is_null() && current_run_id.is_none() {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "conflict",
            "action": "kanban-reclaim",
            "ok": false,
            "task_id": task_id,
            "reason": "task is not in a reclaimable state",
            "nativeRecoveryAction": true
        }));
    }

    let stopped = store.stop_managed_processes(Some(task_id), None, None, None, None, true)?;
    let aborted_agent_run = current_run_id
        .as_deref()
        .and_then(|run_id| {
            store
                .abort_agent_run(
                    run_id,
                    Some(
                        reason
                            .map(|value| format!("Kanban dashboard reclaim: {value}"))
                            .unwrap_or_else(|| "Kanban dashboard reclaim".into()),
                    ),
                )
                .ok()
        })
        .map(|run| {
            json!({
                "run_id": run.run_id,
                "status": run.state,
                "summary": run.last_activity_desc.or(run.error)
            })
        });

    let now = now_iso();
    task["status"] = json!("ready");
    task["updatedAt"] = json!(now.clone());
    task["claimLock"] = Value::Null;
    task["claim_lock"] = Value::Null;
    task["claimExpires"] = Value::Null;
    task["claim_expires"] = Value::Null;
    task["workerPid"] = Value::Null;
    task["worker_pid"] = Value::Null;
    task["currentRunId"] = Value::Null;
    task["current_run_id"] = Value::Null;
    if let Some(runs) = task.get_mut("runs").and_then(Value::as_array_mut) {
        for run in runs.iter_mut().rev() {
            let matches_current = current_run_id
                .as_deref()
                .is_some_and(|run_id| run.get("run_id").and_then(Value::as_str) == Some(run_id));
            let is_running = run.get("status").and_then(Value::as_str) == Some("running");
            if matches_current || is_running {
                run["status"] = json!("reclaimed");
                run["outcome"] = json!("reclaimed");
                run["ended_at"] = json!(now.clone());
                run["error"] = json!(reason.unwrap_or("manual reclaim"));
                break;
            }
        }
    }
    kanban_push_dashboard_event(
        task,
        "reclaimed",
        json!({
            "manual": true,
            "reason": reason,
            "prev_lock": claim_lock,
            "runId": current_run_id,
            "run_id": current_run_id,
            "stopped": stopped.clone(),
            "source": "dashboard_plugins.kanban-reclaim"
        }),
    );
    let task_snapshot = kanban_dashboard_task_card(task);
    store.set_agent_kanban_tasks(tasks)?;

    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-reclaim",
        "ok": true,
        "task_id": task_id,
        "task": task_snapshot,
        "stopped": stopped,
        "aborted_agent_run": aborted_agent_run,
        "nativeRecoveryAction": true
    }))
}

fn kanban_dashboard_reassign(
    store: &AppStore,
    task_id: &str,
    profile: Option<&str>,
    reclaim_first: bool,
    reason: Option<&str>,
) -> AppResult<Value> {
    if task_id.trim().is_empty() {
        return Ok(kanban_not_found_payload("kanban-reassign", "task", task_id));
    }
    let mut reclaim_result = Value::Null;
    if reclaim_first {
        reclaim_result =
            kanban_dashboard_reclaim(store, task_id, Some(reason.unwrap_or("reassign recovery")))?;
        if reclaim_result.get("ok").and_then(Value::as_bool) != Some(true)
            && reclaim_result.get("status").and_then(Value::as_str) != Some("conflict")
        {
            return Ok(reclaim_result);
        }
    }

    let mut tasks = store.agent_kanban_tasks()?;
    let Some(index) = tasks
        .iter()
        .position(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
    else {
        return Ok(kanban_not_found_payload("kanban-reassign", "task", task_id));
    };
    let task = &mut tasks[index];
    if kanban_dashboard_status(task) == "running"
        && task
            .get("claimLock")
            .or_else(|| task.get("claim_lock"))
            .is_some_and(|value| !value.is_null())
    {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "conflict",
            "action": "kanban-reassign",
            "ok": false,
            "task_id": task_id,
            "assignee": profile,
            "reason": "task is still running; pass reclaimFirst=true to release the claim first",
            "nativeRecoveryAction": true
        }));
    }

    let assignee = profile.map(str::to_string);
    let assignee_value = assignee.clone().map(Value::String).unwrap_or(Value::Null);
    task["assignee"] = assignee_value.clone();
    task["updatedAt"] = json!(now_iso());
    if let Some(metadata) = task.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("queuedAgentRequest");
    }
    kanban_push_dashboard_event(
        task,
        "assigned",
        json!({
            "assignee": assignee_value,
            "reclaimFirst": reclaim_first,
            "reason": reason,
            "source": "dashboard_plugins.kanban-reassign"
        }),
    );
    let task_snapshot = kanban_dashboard_task_card(task);
    store.set_agent_kanban_tasks(tasks)?;

    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-reassign",
        "ok": true,
        "task_id": task_id,
        "assignee": assignee,
        "task": task_snapshot,
        "reclaimFirst": reclaim_first,
        "reclaim": reclaim_result,
        "nativeRecoveryAction": true
    }))
}

fn kanban_dashboard_task_log(store: &AppStore, task_id: &str, limit: usize) -> AppResult<Value> {
    let processes = kanban_managed_processes_for_task(store, task_id)?;
    let mut lines = Vec::new();
    for process in &processes {
        let Some(process_id) = process.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(log) = store.managed_process_log(process_id, 0, limit) {
            for line in log
                .get("lines")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                lines.push(json!({
                    "process_id": process_id,
                    "run_id": process.get("runId").cloned().unwrap_or(Value::Null),
                    "stream": line.get("stream").cloned().unwrap_or(Value::Null),
                    "line": line.get("line").cloned().unwrap_or(Value::Null)
                }));
            }
        }
    }
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": if processes.is_empty() { "not_found" } else { "ok" },
        "action": "kanban-task-log",
        "task_id": task_id,
        "content": lines.iter().filter_map(|line| line.get("line").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        "lines": lines,
        "count": lines.len(),
        "processes": processes,
        "source": "SynthChat ManagedProcess stdout/stderr tails",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_dashboard_attachments(store: &AppStore, task_id: &str) -> AppResult<Value> {
    let Some(task) = store
        .agent_kanban_tasks()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
    else {
        return Ok(kanban_not_found_payload(
            "kanban-attachments",
            "task",
            task_id,
        ));
    };
    let mut attachments = task
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(artifacts) = task
        .get("metadata")
        .and_then(|metadata| metadata.get("artifacts"))
        .and_then(Value::as_array)
    {
        for (index, artifact) in artifacts.iter().enumerate() {
            if let Some(path) = artifact.as_str() {
                attachments.push(json!({
                    "id": format!("artifact-{index}"),
                    "filename": path.rsplit(['/', '\\']).next().unwrap_or(path),
                    "stored_path": path,
                    "source": "metadata.artifacts"
                }));
            }
        }
    }
    let attachment_count = attachments.len();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-attachments",
        "task_id": task_id,
        "attachments": attachments,
        "count": attachment_count,
        "uploadSupported": true,
        "downloadSupported": true,
        "deleteSupported": true,
        "boundary": "SynthChat adapts Hermes multipart upload/download/delete as desktop file-backed dashboard actions over AppStore task attachments; it does not embed FastAPI multipart routes.",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_dashboard_attachment_add(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let task_id = payload_string(payload, &["taskId", "task_id"])
        .ok_or_else(|| AppError::BadRequest("kanban-attachment-add requires taskId".into()))?;
    let uploaded_by = payload_string(payload, &["uploadedBy", "uploaded_by", "author"])
        .unwrap_or_else(|| "dashboard".into());
    let content_type = payload_string(payload, &["contentType", "content_type", "mime"]);
    let mut tasks = store.agent_kanban_tasks()?;
    let task = tasks
        .iter_mut()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
    let filename = payload_string(payload, &["filename", "name"])
        .or_else(|| {
            payload_string(payload, &["sourcePath", "source_path", "path"]).and_then(|path| {
                PathBuf::from(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
        })
        .ok_or_else(|| {
            AppError::BadRequest("kanban-attachment-add requires filename or sourcePath".into())
        })?;
    let safe_name = safe_kanban_attachment_name(&filename)?;
    let dest_dir = kanban_task_attachments_dir(store, task, &task_id);
    fs::create_dir_all(&dest_dir)?;
    let dest_path = unique_kanban_attachment_path(&dest_dir, &safe_name);
    let size = if let Some(source_path) =
        payload_string(payload, &["sourcePath", "source_path", "path"])
    {
        let source = PathBuf::from(source_path);
        if !source.is_file() {
            return Err(AppError::BadRequest(format!(
                "attachment source file not found: {}",
                source.display()
            )));
        }
        fs::copy(&source, &dest_path)?
    } else if let Some(encoded) =
        payload_string(payload, &["contentBase64", "content_base64", "base64"])
    {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                encoded
                    .trim()
                    .strip_prefix("data:")
                    .and_then(|value| value.split_once(",").map(|(_, data)| data))
                    .unwrap_or(encoded.trim()),
            )
            .map_err(|error| AppError::BadRequest(format!("invalid attachment base64: {error}")))?;
        fs::write(&dest_path, &bytes)?;
        bytes.len() as u64
    } else if let Some(content) = payload.get("content").and_then(Value::as_str) {
        fs::write(&dest_path, content.as_bytes())?;
        content.len() as u64
    } else {
        return Err(AppError::BadRequest(
            "kanban-attachment-add requires sourcePath, content, or contentBase64".into(),
        ));
    };
    let id = next_kanban_attachment_id(task);
    let attachment = json!({
        "id": id,
        "task_id": task_id,
        "taskId": task_id,
        "filename": dest_path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or(safe_name),
        "stored_path": dest_path.to_string_lossy().to_string(),
        "storedPath": dest_path.to_string_lossy().to_string(),
        "content_type": content_type,
        "contentType": content_type,
        "size": size,
        "uploaded_by": uploaded_by,
        "uploadedBy": uploaded_by,
        "created_at": now_iso(),
        "createdAt": now_iso(),
        "source": "dashboard_plugins.kanban-attachment-add"
    });
    if let Some(attachments) = task.get_mut("attachments").and_then(Value::as_array_mut) {
        attachments.push(attachment.clone());
    } else {
        task["attachments"] = json!([attachment.clone()]);
    }
    kanban_push_dashboard_event(
        task,
        "attached",
        json!({
            "filename": attachment["filename"].clone(),
            "size": size,
            "by": uploaded_by
        }),
    );
    store.set_agent_kanban_tasks(tasks)?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-attachment-add",
        "attachment": attachment,
        "nativeAttachmentWrite": true
    }))
}

fn kanban_dashboard_attachment_read(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let attachment_id = payload_attachment_id(payload).ok_or_else(|| {
        AppError::BadRequest("kanban-attachment-read requires attachmentId".into())
    })?;
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(1024 * 1024)
        .min(5 * 1024 * 1024) as usize;
    let (attachment, _task) = find_kanban_attachment(store, &attachment_id)?.ok_or_else(|| {
        AppError::BadRequest(format!("kanban attachment not found: {attachment_id}"))
    })?;
    let stored_path = attachment
        .get("stored_path")
        .or_else(|| attachment.get("storedPath"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("attachment stored_path is missing".into()))?;
    let path = PathBuf::from(stored_path);
    if !path.is_file() {
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "missing_file",
            "action": "kanban-attachment-read",
            "attachment": attachment,
            "content": Value::Null,
            "contentBase64": Value::Null
        }));
    }
    let mut bytes = fs::read(&path)?;
    let truncated = bytes.len() > limit;
    if truncated {
        bytes.truncate(limit);
    }
    let text = String::from_utf8(bytes.clone()).ok();
    let content_base64 = if text.is_none() {
        use base64::Engine;
        Some(base64::engine::general_purpose::STANDARD.encode(&bytes))
    } else {
        None
    };
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-attachment-read",
        "attachment": attachment,
        "content": text,
        "contentBase64": content_base64,
        "encoding": if content_base64.is_some() { "base64" } else { "utf-8" },
        "truncated": truncated,
        "nativeAttachmentRead": true
    }))
}

fn kanban_dashboard_attachment_delete(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let attachment_id = payload_attachment_id(payload).ok_or_else(|| {
        AppError::BadRequest("kanban-attachment-delete requires attachmentId".into())
    })?;
    let delete_file = payload
        .get("deleteFile")
        .or_else(|| payload.get("delete_file"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut tasks = store.agent_kanban_tasks()?;
    for task in &mut tasks {
        let Some(attachments) = task.get_mut("attachments").and_then(Value::as_array_mut) else {
            continue;
        };
        let Some(position) = attachments
            .iter()
            .position(|attachment| kanban_attachment_id_matches(attachment, &attachment_id))
        else {
            continue;
        };
        let removed = attachments.remove(position);
        if delete_file {
            if let Some(path) = removed
                .get("stored_path")
                .or_else(|| removed.get("storedPath"))
                .and_then(Value::as_str)
            {
                let _ = fs::remove_file(path);
            }
        }
        kanban_push_dashboard_event(
            task,
            "attachment_removed",
            json!({"filename": removed.get("filename").cloned().unwrap_or(Value::Null)}),
        );
        store.set_agent_kanban_tasks(tasks)?;
        return Ok(json!({
            "schema": "hermes_kanban_dashboard_desktop_v1",
            "status": "ok",
            "action": "kanban-attachment-delete",
            "ok": true,
            "id": attachment_id,
            "attachment": removed,
            "fileDeleted": delete_file,
            "nativeAttachmentWrite": true
        }));
    }
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "not_found",
        "action": "kanban-attachment-delete",
        "ok": false,
        "id": attachment_id
    }))
}

fn kanban_dashboard_dispatch(
    store: &AppStore,
    payload: &Value,
    max_spawn: usize,
    dry_run: bool,
    enqueue_agent: bool,
) -> AppResult<Value> {
    let board = payload
        .get("board")
        .and_then(Value::as_str)
        .map(normalize_kanban_board_slug)
        .filter(|value| !value.is_empty());
    let max_spawn = max_spawn.max(1);
    let mut tasks = store.agent_kanban_tasks()?;
    let mut candidate_indices = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            kanban_dashboard_status(task) == "ready"
                && board
                    .as_deref()
                    .is_none_or(|board| kanban_board_for_task(task) == board)
                && task
                    .get("assignee")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    candidate_indices.sort_by(|left, right| {
        let left_task = &tasks[*left];
        let right_task = &tasks[*right];
        kanban_task_priority(right_task)
            .cmp(&kanban_task_priority(left_task))
            .then_with(|| {
                kanban_task_created_at(left_task).cmp(&kanban_task_created_at(right_task))
            })
    });
    candidate_indices.truncate(max_spawn);

    let candidates = candidate_indices
        .iter()
        .map(|index| {
            let task = &tasks[*index];
            json!({
                "task_id": task.get("id").cloned().unwrap_or(Value::Null),
                "assignee": task.get("assignee").cloned().unwrap_or(Value::Null),
                "title": task.get("title").cloned().unwrap_or(Value::Null),
                "would_spawn": !dry_run,
                "reason": if dry_run { "dry-run readiness preview" } else { "desktop dispatcher claim candidate" }
            })
        })
        .collect::<Vec<_>>();

    let mut spawned = Vec::new();
    if !dry_run {
        for index in candidate_indices {
            let Some(task) = tasks.get_mut(index) else {
                continue;
            };
            let task_id = task
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if task_id.is_empty() {
                continue;
            }
            let assignee = task
                .get("assignee")
                .and_then(Value::as_str)
                .unwrap_or("kanban")
                .to_string();
            let title = task
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Kanban task")
                .to_string();
            let run_id = new_id("run-kanban");
            let process_id = new_id("kanban-worker");
            let now = now_iso();
            let claim_lock = format!("synthchat-desktop:{process_id}");
            let conversation_id = task
                .get("conversationId")
                .or_else(|| task.get("conversation_id"))
                .or_else(|| {
                    task.get("metadata")
                        .and_then(|metadata| metadata.get("conversationId"))
                })
                .or_else(|| {
                    task.get("metadata")
                        .and_then(|metadata| metadata.get("conversation_id"))
                })
                .and_then(Value::as_str)
                .unwrap_or("kanban-dashboard-dispatch")
                .to_string();
            let mut run =
                AgentRunRecord::new(conversation_id.clone(), assignee.clone(), assignee.clone());
            run.run_id = run_id.clone();
            run.state = "running".into();
            run.user_request = format!("Kanban dispatch claimed task {task_id}: {title}");
            run.last_activity_at = Some(now.clone());
            run.last_activity_desc = Some("kanban dispatch claimed ready task".into());
            store.save_agent_run(run)?;

            task["status"] = json!("running");
            task["updatedAt"] = json!(now.clone());
            task["startedAt"] = json!(now.clone());
            task["claimLock"] = json!(claim_lock);
            task["claim_lock"] = task["claimLock"].clone();
            task["claimExpires"] = Value::Null;
            task["claim_expires"] = Value::Null;
            task["currentRunId"] = json!(run_id.clone());
            task["current_run_id"] = task["currentRunId"].clone();
            let claim_lock_value = task["claimLock"].clone();
            kanban_push_dashboard_event(
                task,
                "claimed",
                json!({
                    "runId": run_id.clone(),
                    "run_id": run_id.clone(),
                    "processId": process_id.clone(),
                    "process_id": process_id.clone(),
                    "assignee": assignee.clone(),
                    "claimLock": claim_lock_value.clone(),
                    "source": "dashboard_plugins.kanban-dispatch"
                }),
            );
            if let Some(runs) = task.get_mut("runs").and_then(Value::as_array_mut) {
                runs.push(json!({
                    "run_id": run_id.clone(),
                    "status": "running",
                    "assignee": assignee.clone(),
                    "started_at": now,
                    "claim_lock": claim_lock_value.clone(),
                    "worker_pid": Value::Null
                }));
            } else {
                task["runs"] = json!([{
                    "run_id": run_id.clone(),
                    "status": "running",
                    "assignee": assignee.clone(),
                    "started_at": now,
                    "claim_lock": claim_lock_value.clone(),
                    "worker_pid": Value::Null
                }]);
            }

            let process = ManagedProcess {
                id: process_id.clone(),
                label: assignee.clone(),
                command: format!("synthchat kanban worker {task_id}"),
                cwd: task
                    .get("workspacePath")
                    .or_else(|| task.get("workspace_path"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pid: None,
                backend: "kanban".into(),
                env_type: "desktop".into(),
                status_command: Some(vec!["cmd".into(), "/C".into(), "exit".into(), "0".into()]),
                kill_command: Some(vec!["cmd".into(), "/C".into(), "exit".into(), "0".into()]),
                stdout_command: None,
                stderr_command: None,
                exit_command: None,
                cleanup_command: None,
                exit_code: None,
                task_id: Some(task_id.clone()),
                conversation_id,
                run_id: run_id.clone(),
                detached: true,
                pid_scope: "desktop-virtual".into(),
                started_at: now.clone(),
                finished_at: None,
                finished_at_instant: None,
                notify_on_complete: false,
                watch_patterns: Vec::new(),
                tail_retention_lines: 200,
                notifications: Arc::new(Mutex::new(ManagedProcessNotificationState::default())),
                stdout: Arc::new(Mutex::new(vec![format!(
                    "kanban dispatcher claimed {task_id} for {assignee}"
                )])),
                stderr: Arc::new(Mutex::new(Vec::new())),
                stdin: Arc::new(tokio::sync::Mutex::new(None)),
                child: None,
            };
            let process_snapshot = store.register_managed_process(process)?;
            let queued_agent = if enqueue_agent {
                Some(kanban_enqueue_claimed_worker(
                    store, task, &task_id, &title, &assignee, &run_id,
                )?)
            } else {
                None
            };
            if let Some(queued) = queued_agent.as_ref() {
                if let Some(metadata) = task.get_mut("metadata").and_then(Value::as_object_mut) {
                    metadata.insert("queuedAgentRequest".into(), queued.clone());
                } else {
                    task["metadata"] = json!({"queuedAgentRequest": queued});
                }
                kanban_push_dashboard_event(
                    task,
                    "queued_agent",
                    json!({
                        "queueItemId": task.get("metadata").and_then(|metadata| metadata.get("queuedAgentRequest")).and_then(|queued| queued.get("id")).cloned().unwrap_or(Value::Null),
                        "runId": run_id.clone(),
                        "source": "dashboard_plugins.kanban-dispatch"
                    }),
                );
            }
            spawned.push(json!({
                "task_id": task_id,
                "run_id": run_id.clone(),
                "process": process_snapshot,
                "claim_lock": task["claimLock"].clone(),
                "queuedAgent": queued_agent
            }));
        }
        store.set_agent_kanban_tasks(tasks)?;
    }

    let candidate_count = candidates.len();
    let spawned_count = spawned.len();
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-dispatch",
        "dry_run": dry_run,
        "spawned": spawned_count,
        "candidates": candidates,
        "spawnedTasks": spawned,
        "count": candidate_count,
        "board": board,
        "max_spawn": max_spawn,
        "nativeDispatcherClaim": !dry_run,
        "enqueueAgent": enqueue_agent,
        "nativeQueueBridge": enqueue_agent && !dry_run,
        "boundary": "Hermes kanban dispatch_once is DB-backed and spawns worker processes. SynthChat supports a desktop-native claim/run/process registration path when dryRun=false and can enqueue claimed work into the normal agent queue with enqueueAgent=true; automatic in-tool background execution still remains a runtime bridge boundary.",
        "nativeWorkerVisibility": true
    }))
}

fn kanban_enqueue_claimed_worker(
    store: &AppStore,
    task: &Value,
    task_id: &str,
    title: &str,
    assignee: &str,
    claimed_run_id: &str,
) -> AppResult<Value> {
    let persona = store
        .persona(Some(assignee))
        .or_else(|_| store.persona(None))?;
    let conversation = kanban_worker_conversation(store, task, task_id, title, &persona)?;
    let prompt = kanban_worker_prompt(task, task_id, title, assignee, claimed_run_id);
    let message = store.append_message(ChatMessage::new(
        conversation.id.clone(),
        "user",
        prompt,
        "kanban-dispatch-queue",
    ))?;
    let queued =
        store.enqueue_agent_request(conversation.id.clone(), persona.id.clone(), &message)?;
    Ok(json!({
        "id": queued.id,
        "queueItemId": queued.id,
        "conversationId": conversation.id,
        "conversation_id": conversation.id,
        "messageId": message.id,
        "message_id": message.id,
        "personaId": persona.id,
        "persona_id": persona.id,
        "status": queued.status,
        "content": queued.content,
        "claimedRunId": claimed_run_id,
        "claimed_run_id": claimed_run_id
    }))
}

fn kanban_worker_conversation(
    store: &AppStore,
    task: &Value,
    task_id: &str,
    title: &str,
    persona: &Persona,
) -> AppResult<Conversation> {
    if let Some(existing_id) = task
        .get("conversationId")
        .or_else(|| task.get("conversation_id"))
        .or_else(|| {
            task.get("metadata")
                .and_then(|metadata| metadata.get("conversationId"))
        })
        .or_else(|| {
            task.get("metadata")
                .and_then(|metadata| metadata.get("conversation_id"))
        })
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if let Ok(conversation) = store.conversation(existing_id) {
            return Ok(conversation);
        }
    }
    store.create_conversation(
        Some(format!("Kanban worker {task_id}: {title}")),
        Some(persona.id.clone()),
    )
}

fn kanban_worker_prompt(
    task: &Value,
    task_id: &str,
    title: &str,
    assignee: &str,
    claimed_run_id: &str,
) -> String {
    let body = task
        .get("body")
        .or_else(|| task.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let attachments = task
        .get("attachments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(format!(
                        "- {} ({})",
                        item.get("filename").and_then(Value::as_str)?,
                        item.get("stored_path")
                            .or_else(|| item.get("storedPath"))
                            .and_then(Value::as_str)
                            .unwrap_or("no stored_path")
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "- none".into());
    format!(
        "You are the Kanban worker for task `{task_id}` assigned to `{assignee}`.\n\
Claimed run: `{claimed_run_id}`.\n\n\
Title: {title}\n\n\
Body:\n{body}\n\n\
Attachments:\n{attachments}\n\n\
Work this task to completion. Use the native kanban tools to report lifecycle: call `kanban_complete` with `taskId` and a concise summary when done, or `kanban_block` with `taskId` and a clear reason if blocked. Preserve any useful artifact paths in completion metadata."
    )
}

fn kanban_dashboard_boards(store: &AppStore, include_archived: bool) -> AppResult<Value> {
    let settings = kanban_dashboard_settings(store);
    let current = settings
        .get("currentBoard")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let mut boards = BTreeMap::<String, Value>::new();
    boards.insert(
        "default".into(),
        json!({
            "slug": "default",
            "name": "Default",
            "description": "SynthChat desktop default kanban board",
            "icon": "Kanban",
            "color": "#64748b",
            "archived": false
        }),
    );
    if let Some(config_boards) = settings.get("boards").and_then(Value::as_object) {
        for (slug, meta) in config_boards {
            if include_archived
                || !meta
                    .get("archived")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                boards.insert(slug.clone(), kanban_board_meta(slug, meta));
            }
        }
    }
    let mut counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for task in store.agent_kanban_tasks()? {
        let board = kanban_board_for_task(&task);
        if !boards.contains_key(&board) {
            boards.insert(
                board.clone(),
                json!({
                    "slug": board,
                    "name": board,
                    "description": "",
                    "icon": "Kanban",
                    "color": "#64748b",
                    "archived": false,
                    "derivedFromTasks": true
                }),
            );
        }
        *counts
            .entry(board)
            .or_default()
            .entry(kanban_dashboard_status(&task))
            .or_default() += 1;
    }
    let mut board_values = Vec::new();
    for (slug, mut meta) in boards {
        let board_counts = counts.remove(&slug).unwrap_or_default();
        let total = board_counts.values().sum::<usize>();
        meta["counts"] = json!(board_counts);
        meta["total"] = json!(total);
        meta["is_current"] = json!(slug == current);
        board_values.push(meta);
    }
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-boards",
        "boards": board_values,
        "current": current,
        "source": "SynthChat config.chat.auxiliary_task_assignments.kanbanDashboard + AppStore.agent_kanban_tasks",
        "nativeBoardSettings": true
    }))
}

fn kanban_dashboard_board_create(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let slug = kanban_slug_arg(payload, &["slug", "board"]).unwrap_or_else(|| "default".into());
    kanban_update_settings(store, |settings| {
        let boards = settings
            .as_object_mut()
            .expect("settings object")
            .entry("boards")
            .or_insert_with(|| json!({}));
        let board = boards
            .as_object_mut()
            .expect("boards object")
            .entry(slug.clone())
            .or_insert_with(|| json!({}));
        apply_kanban_board_meta_patch(board, payload, &slug);
        if payload
            .get("switch")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            settings["currentBoard"] = json!(slug.clone());
        }
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-board-create",
        "board": kanban_config_board(store, &slug),
        "current": kanban_dashboard_settings(store).get("currentBoard").cloned().unwrap_or_else(|| json!("default")),
        "nativeBoardSettings": true
    }))
}

fn kanban_dashboard_board_update(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let slug = kanban_slug_arg(payload, &["slug", "board"]).unwrap_or_else(|| "default".into());
    kanban_update_settings(store, |settings| {
        let boards = settings
            .as_object_mut()
            .expect("settings object")
            .entry("boards")
            .or_insert_with(|| json!({}));
        let board = boards
            .as_object_mut()
            .expect("boards object")
            .entry(slug.clone())
            .or_insert_with(|| json!({}));
        apply_kanban_board_meta_patch(board, payload, &slug);
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-board-update",
        "board": kanban_config_board(store, &slug),
        "nativeBoardSettings": true
    }))
}

fn kanban_dashboard_board_delete(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let slug = kanban_slug_arg(payload, &["slug", "board"]).unwrap_or_else(|| "default".into());
    let hard_delete = payload
        .get("hardDelete")
        .or_else(|| payload.get("delete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    kanban_update_settings(store, |settings| {
        if let Some(boards) = settings.get_mut("boards").and_then(Value::as_object_mut) {
            if hard_delete {
                boards.remove(&slug);
            } else {
                let board = boards.entry(slug.clone()).or_insert_with(|| json!({}));
                board["archived"] = json!(true);
                board["slug"] = json!(slug.clone());
            }
        }
        if settings.get("currentBoard").and_then(Value::as_str) == Some(slug.as_str()) {
            settings["currentBoard"] = json!("default");
        }
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-board-delete",
        "result": {"slug": slug, "deleted": hard_delete, "archived": !hard_delete},
        "current": kanban_dashboard_settings(store).get("currentBoard").cloned().unwrap_or_else(|| json!("default")),
        "nativeBoardSettings": true
    }))
}

fn kanban_dashboard_board_switch(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let slug = kanban_slug_arg(payload, &["slug", "board"]).unwrap_or_else(|| "default".into());
    kanban_update_settings(store, |settings| {
        settings["currentBoard"] = json!(slug.clone());
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-board-switch",
        "current": slug,
        "nativeBoardSettings": true
    }))
}

fn kanban_dashboard_profiles(store: &AppStore) -> AppResult<Value> {
    let settings = kanban_dashboard_settings(store);
    let descriptions = settings
        .get("profileDescriptions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let profile = store.profile()?;
    let personas = store.personas()?;
    let mut profiles = vec![json!({
        "name": "default",
        "is_default": true,
        "model": "",
        "provider": "",
        "description": descriptions.get("default").and_then(Value::as_str).unwrap_or("SynthChat desktop active profile"),
        "description_auto": false,
        "skill_count": 0,
        "display_name": profile.name
    })];
    for persona in personas {
        profiles.push(json!({
            "name": persona.id,
            "is_default": false,
            "model": persona.llm_model,
            "provider": persona.llm_provider,
            "description": descriptions.get(&persona.id).and_then(Value::as_str).unwrap_or(""),
            "description_auto": settings.get("profileDescriptionAuto").and_then(|value| value.get(&persona.id)).and_then(Value::as_bool).unwrap_or(false),
            "skill_count": 0,
            "display_name": persona.name
        }));
    }
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-profiles",
        "profiles": profiles,
        "source": "SynthChat profile + personas",
        "nativeOrchestrationSettings": true
    }))
}

fn kanban_dashboard_profile_update(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let profile_name = payload
        .get("profile")
        .or_else(|| payload.get("profileName"))
        .or_else(|| payload.get("profile_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string();
    let description = payload
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    kanban_update_settings(store, |settings| {
        let descriptions = settings
            .as_object_mut()
            .expect("settings object")
            .entry("profileDescriptions")
            .or_insert_with(|| json!({}));
        descriptions
            .as_object_mut()
            .expect("descriptions object")
            .insert(profile_name.clone(), Value::String(description.clone()));
        let auto = settings
            .as_object_mut()
            .expect("settings object")
            .entry("profileDescriptionAuto")
            .or_insert_with(|| json!({}));
        auto.as_object_mut()
            .expect("profile auto object")
            .insert(profile_name.clone(), Value::Bool(false));
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-profile-update",
        "ok": true,
        "profile": profile_name,
        "description": description,
        "nativeOrchestrationSettings": true
    }))
}

fn kanban_dashboard_profile_describe_auto(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let profile_name = payload
        .get("profile")
        .or_else(|| payload.get("profileName"))
        .or_else(|| payload.get("profile_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string();
    let description = format!("SynthChat desktop profile for Kanban routing: {profile_name}");
    kanban_update_settings(store, |settings| {
        settings
            .as_object_mut()
            .expect("settings object")
            .entry("profileDescriptions")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("descriptions object")
            .insert(profile_name.clone(), json!(description.clone()));
        settings
            .as_object_mut()
            .expect("settings object")
            .entry("profileDescriptionAuto")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("profile auto object")
            .insert(profile_name.clone(), json!(true));
    })?;
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-profile-describe-auto",
        "ok": true,
        "profile": profile_name,
        "reason": "desktop_deterministic",
        "description": description,
        "nativeOrchestrationSettings": true
    }))
}

fn kanban_dashboard_orchestration(store: &AppStore) -> AppResult<Value> {
    let settings = kanban_dashboard_settings(store);
    let orchestration = settings
        .get("orchestration")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let active_profile = "default";
    let orchestrator = orchestration
        .get("orchestrator_profile")
        .and_then(Value::as_str)
        .unwrap_or(active_profile);
    let default_assignee = orchestration
        .get("default_assignee")
        .and_then(Value::as_str)
        .unwrap_or(active_profile);
    Ok(json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "ok",
        "action": "kanban-orchestration",
        "orchestrator_profile": orchestration.get("orchestrator_profile").and_then(Value::as_str).unwrap_or(""),
        "default_assignee": orchestration.get("default_assignee").and_then(Value::as_str).unwrap_or(""),
        "auto_decompose": orchestration.get("auto_decompose").and_then(Value::as_bool).unwrap_or(true),
        "auto_promote_children": orchestration.get("auto_promote_children").and_then(Value::as_bool).unwrap_or(true),
        "resolved_orchestrator_profile": orchestrator,
        "resolved_default_assignee": default_assignee,
        "active_profile": active_profile,
        "source": "config.chat.auxiliary_task_assignments.kanbanDashboard.orchestration",
        "nativeOrchestrationSettings": true
    }))
}

fn kanban_dashboard_orchestration_set(store: &AppStore, payload: &Value) -> AppResult<Value> {
    kanban_update_settings(store, |settings| {
        let orchestration = settings
            .as_object_mut()
            .expect("settings object")
            .entry("orchestration")
            .or_insert_with(|| json!({}));
        for key in [
            "orchestrator_profile",
            "default_assignee",
            "auto_decompose",
            "auto_promote_children",
        ] {
            if let Some(value) = payload.get(key) {
                orchestration[key] = value.clone();
            }
        }
        if let Some(value) = payload.get("orchestratorProfile") {
            orchestration["orchestrator_profile"] = value.clone();
        }
        if let Some(value) = payload.get("defaultAssignee") {
            orchestration["default_assignee"] = value.clone();
        }
        if let Some(value) = payload.get("autoDecompose") {
            orchestration["auto_decompose"] = value.clone();
        }
        if let Some(value) = payload.get("autoPromoteChildren") {
            orchestration["auto_promote_children"] = value.clone();
        }
    })?;
    let mut snapshot = kanban_dashboard_orchestration(store)?;
    snapshot["action"] = json!("kanban-orchestration-set");
    snapshot["ok"] = json!(true);
    Ok(snapshot)
}

fn kanban_dashboard_settings(store: &AppStore) -> Value {
    store
        .config()
        .ok()
        .and_then(|config| {
            config
                .chat
                .auxiliary_task_assignments
                .get("kanbanDashboard")
                .cloned()
        })
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn kanban_update_settings<F>(store: &AppStore, update: F) -> AppResult<()>
where
    F: FnOnce(&mut Value),
{
    let mut config = store.config()?;
    if !config.chat.auxiliary_task_assignments.is_object() {
        config.chat.auxiliary_task_assignments = json!({});
    }
    let root = config
        .chat
        .auxiliary_task_assignments
        .as_object_mut()
        .expect("auxiliary_task_assignments object");
    let settings = root.entry("kanbanDashboard").or_insert_with(|| json!({}));
    if !settings.is_object() {
        *settings = json!({});
    }
    update(settings);
    store.set_config(config)
}

fn kanban_slug_arg(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(normalize_kanban_board_slug)
        .filter(|value| !value.is_empty())
}

fn normalize_kanban_board_slug(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                Some(ch)
            } else if ch.is_ascii_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn kanban_config_board(store: &AppStore, slug: &str) -> Value {
    let settings = kanban_dashboard_settings(store);
    settings
        .get("boards")
        .and_then(Value::as_object)
        .and_then(|boards| boards.get(slug))
        .map(|meta| kanban_board_meta(slug, meta))
        .unwrap_or_else(|| {
            json!({
                "slug": slug,
                "name": if slug == "default" { "Default" } else { slug },
                "description": "",
                "icon": "Kanban",
                "color": "#64748b",
                "archived": false
            })
        })
}

fn kanban_board_meta(slug: &str, meta: &Value) -> Value {
    json!({
        "slug": slug,
        "name": meta.get("name").and_then(Value::as_str).unwrap_or(slug),
        "description": meta.get("description").and_then(Value::as_str).unwrap_or(""),
        "icon": meta.get("icon").and_then(Value::as_str).unwrap_or("Kanban"),
        "color": meta.get("color").and_then(Value::as_str).unwrap_or("#64748b"),
        "archived": meta.get("archived").and_then(Value::as_bool).unwrap_or(false)
    })
}

fn apply_kanban_board_meta_patch(board: &mut Value, payload: &Value, slug: &str) {
    if !board.is_object() {
        *board = json!({});
    }
    board["slug"] = json!(slug);
    for key in ["name", "description", "icon", "color"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            board[key] = json!(value.trim());
        }
    }
    if let Some(archived) = payload.get("archived").and_then(Value::as_bool) {
        board["archived"] = json!(archived);
    } else if board.get("archived").is_none() {
        board["archived"] = json!(false);
    }
}

fn kanban_board_for_task(task: &Value) -> String {
    task.get("board")
        .or_else(|| {
            task.get("metadata")
                .and_then(|metadata| metadata.get("board"))
        })
        .and_then(Value::as_str)
        .map(normalize_kanban_board_slug)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into())
}

fn kanban_managed_process_by_run(store: &AppStore, run_id: &str) -> AppResult<Option<Value>> {
    Ok(store
        .managed_processes()?
        .into_iter()
        .find(|process| process.get("runId").and_then(Value::as_str) == Some(run_id)))
}

fn kanban_managed_processes_for_task(store: &AppStore, task_id: &str) -> AppResult<Vec<Value>> {
    Ok(store
        .managed_processes()?
        .into_iter()
        .filter(|process| process.get("taskId").and_then(Value::as_str) == Some(task_id))
        .collect())
}

fn kanban_run_from_process(process: &Value) -> Value {
    json!({
        "run_id": process.get("runId").cloned().unwrap_or(Value::Null),
        "task_id": process.get("taskId").cloned().unwrap_or(Value::Null),
        "status": process.get("status").cloned().unwrap_or(Value::Null),
        "profile": process.get("label").cloned().unwrap_or(Value::Null),
        "worker_pid": process.get("pid").cloned().unwrap_or(Value::Null),
        "started_at": process.get("startedAt").cloned().unwrap_or(Value::Null),
        "ended_at": process.get("finishedAt").cloned().unwrap_or(Value::Null),
        "claim_lock": process.get("id").cloned().unwrap_or(Value::Null),
        "summary": process.get("note").cloned().unwrap_or(Value::Null)
    })
}

fn kanban_task_title_for_id(store: &AppStore, task_id: Option<&str>) -> Option<Value> {
    kanban_task_field_for_id(store, task_id, "title")
}

fn kanban_task_status_for_id(store: &AppStore, task_id: Option<&str>) -> Option<Value> {
    let task_id = task_id?;
    store
        .agent_kanban_tasks()
        .ok()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
        .map(|task| json!(kanban_dashboard_status(&task)))
}

fn kanban_task_assignee_for_id(store: &AppStore, task_id: Option<&str>) -> Option<Value> {
    kanban_task_field_for_id(store, task_id, "assignee")
}

fn kanban_task_field_for_id(store: &AppStore, task_id: Option<&str>, field: &str) -> Option<Value> {
    let task_id = task_id?;
    store
        .agent_kanban_tasks()
        .ok()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
        .and_then(|task| task.get(field).cloned())
}

fn kanban_not_found_payload(action: &str, kind: &str, id: &str) -> Value {
    json!({
        "schema": "hermes_kanban_dashboard_desktop_v1",
        "status": "not_found",
        "action": action,
        "kind": kind,
        "id": id,
        "nativeWorkerVisibility": true
    })
}

fn kanban_severity_matches(severity: Option<&str>, filter: &str) -> bool {
    let Some(severity) = severity else {
        return false;
    };
    let rank = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "critical" => 3,
        "error" => 2,
        "warning" => 1,
        _ => 0,
    };
    rank(severity) >= rank(filter)
}

fn filtered_kanban_tasks(
    store: &AppStore,
    include_archived: bool,
    tenant: Option<&str>,
) -> AppResult<Vec<Value>> {
    Ok(store
        .agent_kanban_tasks()?
        .into_iter()
        .filter(|task| {
            if !include_archived && kanban_dashboard_status(task) == "archived" {
                return false;
            }
            if let Some(tenant) = tenant {
                return task.get("tenant").and_then(Value::as_str) == Some(tenant);
            }
            true
        })
        .collect())
}

fn kanban_dashboard_task_card(task: &Value) -> Value {
    let parents = task
        .get("parents")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let children = task
        .get("children")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let comments = task
        .get("comments")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let events = task
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "id": task.get("id").cloned().unwrap_or(Value::Null),
        "title": task.get("title").cloned().unwrap_or(Value::Null),
        "body": task.get("body").or_else(|| task.get("description")).cloned().unwrap_or(Value::Null),
        "status": kanban_dashboard_status(task),
        "synthChatStatus": task.get("status").cloned().unwrap_or(Value::Null),
        "assignee": task.get("assignee").cloned().unwrap_or(Value::Null),
        "priority": task.get("priority").cloned().unwrap_or(Value::Null),
        "tenant": task.get("tenant").cloned().unwrap_or(Value::Null),
        "created_at": task.get("createdAt").or_else(|| task.get("created_at")).cloned().unwrap_or(Value::Null),
        "updated_at": task.get("updatedAt").or_else(|| task.get("updated_at")).cloned().unwrap_or(Value::Null),
        "started_at": task.get("startedAt").or_else(|| task.get("started_at")).cloned().unwrap_or(Value::Null),
        "completed_at": task.get("completedAt").or_else(|| task.get("completed_at")).cloned().unwrap_or(Value::Null),
        "block_reason": task.get("blockReason").or_else(|| task.get("block_reason")).cloned().unwrap_or(Value::Null),
        "metadata": task.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "link_counts": {"parents": parents, "children": children},
        "comment_count": comments,
        "event_count": events,
        "progress": if children > 0 { json!({"done": 0, "total": children}) } else { Value::Null },
        "diagnostics": [],
        "warnings": []
    })
}

fn kanban_dashboard_column_for_task(task: &Value) -> String {
    let status = kanban_dashboard_status(task);
    if KANBAN_DASHBOARD_COLUMNS.contains(&status.as_str()) || status == "archived" {
        status
    } else {
        "todo".into()
    }
}

fn kanban_dashboard_status(task: &Value) -> String {
    match task
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("todo")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "completed" => "done".into(),
        "in_progress" => "running".into(),
        "" => "todo".into(),
        other => other.into(),
    }
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone)]
struct KanbanHomeChannel {
    platform: String,
    chat_id: String,
    thread_id: Option<String>,
    name: String,
}

fn configured_kanban_home_channels(store: &AppStore) -> AppResult<Vec<KanbanHomeChannel>> {
    let config = store.config()?;
    let candidates: Vec<(&str, &Value, &[&str], &[&str])> = vec![
        (
            "discord",
            &config.discord,
            &["DISCORD_HOME_CHANNEL"],
            &["DISCORD_HOME_CHANNEL_THREAD_ID"],
        ),
        (
            "telegram",
            &config.telegram,
            &["TELEGRAM_HOME_CHANNEL"],
            &["TELEGRAM_HOME_CHANNEL_THREAD_ID"],
        ),
        (
            "slack",
            &config.slack,
            &["SLACK_HOME_CHANNEL"],
            &["SLACK_HOME_CHANNEL_THREAD_ID"],
        ),
        (
            "mattermost",
            &config.mattermost,
            &["MATTERMOST_HOME_CHANNEL"],
            &["MATTERMOST_HOME_CHANNEL_THREAD_ID"],
        ),
        (
            "teams",
            &config.teams,
            &["TEAMS_HOME_CHANNEL"],
            &["TEAMS_HOME_CHANNEL_THREAD_ID"],
        ),
        ("ntfy", &config.ntfy, &["NTFY_HOME_CHANNEL"], &[]),
        ("simplex", &config.simplex, &["SIMPLEX_HOME_CHANNEL"], &[]),
        ("irc", &config.irc, &["IRC_HOME_CHANNEL"], &[]),
        ("line", &config.line, &["LINE_HOME_CHANNEL"], &[]),
        (
            "google_chat",
            &config.google_chat,
            &["GOOGLE_CHAT_HOME_CHANNEL"],
            &[],
        ),
        (
            "feishu",
            &config.feishu,
            &["FEISHU_HOME_CHANNEL"],
            &["FEISHU_HOME_CHANNEL_THREAD_ID"],
        ),
    ];
    let mut homes = candidates
        .into_iter()
        .filter_map(|(platform, value, env_keys, thread_env_keys)| {
            let chat_id = config_home_channel(value)
                .or_else(|| env_keys.iter().find_map(|key| env::var(key).ok()))?;
            let thread_id = config_home_thread(value).or_else(|| {
                thread_env_keys
                    .iter()
                    .find_map(|key| env::var(key).ok())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            });
            Some(KanbanHomeChannel {
                platform: platform.into(),
                chat_id,
                thread_id,
                name: config_home_name(value).unwrap_or_else(|| "Home".into()),
            })
        })
        .collect::<Vec<_>>();
    homes.sort_by(|left, right| left.platform.cmp(&right.platform));
    Ok(homes)
}

fn config_home_channel(value: &Value) -> Option<String> {
    payload_string(
        value,
        &[
            "homeChannel",
            "home_channel",
            "homeChannelId",
            "home_channel_id",
            "chatId",
            "chat_id",
            "channelId",
            "channel_id",
            "receiveId",
            "receive_id",
            "topic",
            "roomId",
            "room_id",
            "space",
            "target",
        ],
    )
    .or_else(|| nested_home_channel_field(value, &["chat_id", "chatId", "channel_id", "channelId"]))
}

fn config_home_thread(value: &Value) -> Option<String> {
    payload_string(
        value,
        &[
            "homeThread",
            "home_thread",
            "homeThreadId",
            "home_thread_id",
            "homeChannelThreadId",
            "home_channel_thread_id",
            "threadId",
            "thread_id",
            "threadTs",
            "thread_ts",
        ],
    )
    .or_else(|| {
        nested_home_channel_field(value, &["thread_id", "threadId", "thread_ts", "threadTs"])
    })
}

fn config_home_name(value: &Value) -> Option<String> {
    payload_string(value, &["homeName", "home_name", "name"])
        .or_else(|| nested_home_channel_field(value, &["name", "title"]))
}

fn nested_home_channel_field(value: &Value, keys: &[&str]) -> Option<String> {
    let home = value
        .get("homeChannel")
        .or_else(|| value.get("home_channel"))
        .filter(|value| value.is_object())?;
    payload_string(home, keys)
}

fn kanban_task_notify_subs(store: &AppStore, task_id: &str) -> AppResult<Vec<Value>> {
    Ok(store
        .agent_kanban_tasks()?
        .into_iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
        .and_then(|task| {
            task.get("metadata")
                .and_then(|metadata| {
                    metadata
                        .get("notifySubs")
                        .or_else(|| metadata.get("notify_subs"))
                })
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default())
}

fn kanban_home_sub_matches(sub: &Value, home: &KanbanHomeChannel) -> bool {
    sub.get("platform").and_then(Value::as_str) == Some(home.platform.as_str())
        && sub
            .get("chat_id")
            .or_else(|| sub.get("chatId"))
            .and_then(Value::as_str)
            == Some(home.chat_id.as_str())
        && sub
            .get("thread_id")
            .or_else(|| sub.get("threadId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            == home.thread_id.as_deref().unwrap_or_default()
}

fn active_kanban_profile_name(store: &AppStore) -> String {
    store
        .personas()
        .ok()
        .and_then(|personas| personas.into_iter().next().map(|persona| persona.id))
        .unwrap_or_else(|| "default".into())
}

fn safe_kanban_attachment_name(raw: &str) -> AppResult<String> {
    let leaf = raw.replace('\\', "/");
    let name = leaf
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control() && *ch != '\0')
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    if name.is_empty() {
        return Err(AppError::BadRequest("invalid attachment filename".into()));
    }
    Ok(name)
}

fn kanban_task_attachments_dir(store: &AppStore, task: &Value, task_id: &str) -> PathBuf {
    let root = if kanban_board_for_task(task) == "default" {
        hermes_home(store).join("kanban").join("attachments")
    } else {
        hermes_home(store)
            .join("kanban")
            .join("boards")
            .join(kanban_board_for_task(task))
            .join("attachments")
    };
    root.join(task_id)
}

fn unique_kanban_attachment_path(dir: &PathBuf, safe_name: &str) -> PathBuf {
    let initial = dir.join(safe_name);
    if !initial.exists() {
        return initial;
    }
    let path = PathBuf::from(safe_name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| safe_name.to_string());
    let ext = path
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    for index in 1..1000 {
        let candidate = dir.join(format!("{stem} ({index}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({}){ext}", new_id("att")))
}

fn next_kanban_attachment_id(task: &Value) -> u64 {
    task.get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attachment| attachment.get("id").and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
        + 1
}

fn payload_attachment_id(payload: &Value) -> Option<String> {
    payload
        .get("attachmentId")
        .or_else(|| payload.get("attachment_id"))
        .or_else(|| payload.get("id"))
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_u64().map(|number| number.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn kanban_attachment_id_matches(attachment: &Value, id: &str) -> bool {
    attachment.get("id").is_some_and(|value| {
        value.as_str().is_some_and(|value| value == id)
            || value.as_u64().is_some_and(|value| value.to_string() == id)
    })
}

fn find_kanban_attachment(
    store: &AppStore,
    attachment_id: &str,
) -> AppResult<Option<(Value, Value)>> {
    for task in store.agent_kanban_tasks()? {
        if let Some(attachment) = task
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|attachment| kanban_attachment_id_matches(attachment, attachment_id))
        {
            return Ok(Some((attachment.clone(), task)));
        }
    }
    Ok(None)
}

fn kanban_task_priority(task: &Value) -> i64 {
    task.get("priority").and_then(Value::as_i64).unwrap_or(0)
}

fn kanban_task_created_at(task: &Value) -> String {
    task.get("createdAt")
        .or_else(|| task.get("created_at"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn kanban_push_dashboard_event(task: &mut Value, kind: &str, payload: Value) {
    let event = json!({
        "kind": kind,
        "payload": payload,
        "createdAt": now_iso()
    });
    if let Some(events) = task.get_mut("events").and_then(Value::as_array_mut) {
        events.push(event);
    } else {
        task["events"] = json!([event]);
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn write_json_file(path: &PathBuf, value: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn dashboard_achievements_paths(store: &AppStore) -> (PathBuf, PathBuf, PathBuf) {
    let root = hermes_home(store)
        .join("plugins")
        .join("hermes-achievements");
    (
        root.join("state.json"),
        root.join("scan_snapshot.json"),
        root.join("scan_checkpoint.json"),
    )
}

fn dashboard_achievements_payload(store: &AppStore) -> AppResult<Value> {
    let (_, snapshot_path, _) = dashboard_achievements_paths(store);
    let snapshot = read_json_file(&snapshot_path)
        .filter(|value| value.get("achievements").is_some())
        .unwrap_or(dashboard_achievements_rescan(store)?["achievements"].clone());
    let generated_at = snapshot
        .get("generated_at")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now = Utc::now().timestamp();
    let stale = generated_at <= 0
        || now.saturating_sub(generated_at) > ACHIEVEMENT_SNAPSHOT_TTL_SECONDS as i64;
    Ok(json!({
        "achievements": snapshot.get("achievements").cloned().unwrap_or_else(|| json!([])),
        "unlocked_count": snapshot.get("unlocked_count").cloned().unwrap_or_else(|| json!(0)),
        "discovered_count": snapshot.get("discovered_count").cloned().unwrap_or_else(|| json!(0)),
        "secret_count": snapshot.get("secret_count").cloned().unwrap_or_else(|| json!(0)),
        "total_count": snapshot.get("total_count").cloned().unwrap_or_else(|| json!(ACHIEVEMENT_COUNT)),
        "error": snapshot.get("error").cloned().unwrap_or(Value::Null),
        "generated_at": snapshot.get("generated_at").cloned().unwrap_or(Value::Null),
        "is_stale": stale,
        "scan_meta": {
            "mode": snapshot.get("scan_meta").and_then(|meta| meta.get("mode")).cloned().unwrap_or_else(|| json!("desktop")),
            "status": dashboard_achievements_scan_status(store)
        },
        "nativeDashboardRoute": true,
        "schema": "hermes_achievements_dashboard_desktop_v1"
    }))
}

fn dashboard_achievements_scan_status(store: &AppStore) -> Value {
    let (_, snapshot_path, checkpoint_path) = dashboard_achievements_paths(store);
    let snapshot = read_json_file(&snapshot_path);
    let checkpoint = read_json_file(&checkpoint_path);
    let generated_at = snapshot
        .as_ref()
        .and_then(|value| value.get("generated_at"))
        .and_then(Value::as_i64);
    let now = Utc::now().timestamp();
    json!({
        "state": "idle",
        "started_at": Value::Null,
        "finished_at": generated_at,
        "last_error": snapshot.as_ref().and_then(|value| value.get("error")).cloned().unwrap_or(Value::Null),
        "last_duration_ms": Value::Null,
        "run_count": checkpoint
            .as_ref()
            .and_then(|value| value.get("runs"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "ttl_seconds": ACHIEVEMENT_SNAPSHOT_TTL_SECONDS,
        "snapshot_generated_at": generated_at,
        "snapshot_age_seconds": generated_at.map(|value| now.saturating_sub(value)),
        "snapshot_stale": generated_at
            .map(|value| now.saturating_sub(value) > ACHIEVEMENT_SNAPSHOT_TTL_SECONDS as i64)
            .unwrap_or(true),
        "nativeDashboardRoute": true
    })
}

fn dashboard_achievements_rescan(store: &AppStore) -> AppResult<Value> {
    let (state_path, snapshot_path, checkpoint_path) = dashboard_achievements_paths(store);
    let scan = scan_synthchat_achievement_history(store)?;
    let now = chrono::Utc::now().timestamp();
    let existing_state = read_json_file(&state_path).unwrap_or_else(|| json!({"unlocks": {}}));
    let mut unlocks = existing_state
        .get("unlocks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut achievements = achievement_evaluations(&scan["aggregate"]);
    if let Some(items) = achievements.as_array_mut() {
        for item in items.iter_mut() {
            let unlocked = item
                .get("unlocked")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if unlocked && !unlocks.contains_key(&id) {
                unlocks.insert(
                    id.clone(),
                    json!({
                        "unlocked_at": now,
                        "first_tier": item.get("tier").cloned().unwrap_or(Value::Null),
                        "evidence": item.get("evidence").cloned().unwrap_or(Value::Null)
                    }),
                );
            }
            if unlocked {
                if let Some(stored) = unlocks.get(&id) {
                    item["unlocked_at"] = stored
                        .get("unlocked_at")
                        .cloned()
                        .unwrap_or_else(|| json!(now));
                    item["evidence"] = stored
                        .get("evidence")
                        .cloned()
                        .unwrap_or_else(|| item.get("evidence").cloned().unwrap_or(Value::Null));
                }
            }
        }
    }
    let unlocked_count = achievements
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("unlocked")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    let discovered_count = achievements
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("state")
                        .and_then(Value::as_str)
                        .is_some_and(|state| state == "discovered")
                })
                .count()
        })
        .unwrap_or(0);
    let secret_count = achievements
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("state")
                        .and_then(Value::as_str)
                        .is_some_and(|state| state == "secret")
                })
                .count()
        })
        .unwrap_or(0);
    let snapshot = json!({
        "schema": "hermes_achievements_snapshot_desktop_v1",
        "source": "synthchat_app_store",
        "achievements": achievements,
        "sessions": scan["sessions"].clone(),
        "aggregate": scan["aggregate"].clone(),
        "scan_meta": scan["scan_meta"].clone(),
        "error": null,
        "unlocked_count": unlocked_count,
        "discovered_count": discovered_count,
        "secret_count": secret_count,
        "total_count": ACHIEVEMENT_COUNT,
        "generated_at": now
    });
    let checkpoint = json!({
        "schema_version": 1,
        "generated_at": now,
        "sessions": scan["checkpoint_sessions"].clone()
    });
    let state = json!({"unlocks": unlocks});
    write_json_file(&state_path, &state)?;
    write_json_file(&snapshot_path, &snapshot)?;
    write_json_file(&checkpoint_path, &checkpoint)?;

    Ok(json!({
        "schema": "hermes_dashboard_plugins_desktop_v1",
        "status": "ok",
        "action": "rescan",
        "ok": true,
        "achievements": snapshot,
        "statePath": state_path.to_string_lossy().to_string(),
        "snapshotPath": snapshot_path.to_string_lossy().to_string(),
        "checkpointPath": checkpoint_path.to_string_lossy().to_string(),
        "desktopNativeScan": true,
        "fastApiHostEmbedded": false
    }))
}

fn dashboard_achievements_reset_state(store: &AppStore) -> AppResult<Value> {
    let (state_path, snapshot_path, checkpoint_path) = dashboard_achievements_paths(store);
    write_json_file(&state_path, &json!({"unlocks": {}}))?;
    let snapshot_removed = fs::remove_file(&snapshot_path).is_ok();
    let checkpoint_removed = fs::remove_file(&checkpoint_path).is_ok();
    Ok(json!({
        "schema": "hermes_dashboard_plugins_desktop_v1",
        "status": "ok",
        "action": "reset-state",
        "ok": true,
        "statePath": state_path.to_string_lossy().to_string(),
        "snapshotPath": snapshot_path.to_string_lossy().to_string(),
        "checkpointPath": checkpoint_path.to_string_lossy().to_string(),
        "stateReset": true,
        "snapshotRemoved": snapshot_removed,
        "checkpointRemoved": checkpoint_removed
    }))
}

fn dashboard_achievements_recent_unlocks(store: &AppStore, limit: usize) -> Value {
    let (_, snapshot_path, _) = dashboard_achievements_paths(store);
    let mut unlocked = read_json_file(&snapshot_path)
        .and_then(|snapshot| {
            snapshot
                .get("achievements")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|item| {
            item.get("unlocked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    unlocked.sort_by(|left, right| {
        right
            .get("unlocked_at")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&left.get("unlocked_at").and_then(Value::as_i64).unwrap_or(0))
    });
    unlocked.truncate(limit);
    json!({
        "schema": "hermes_dashboard_plugins_desktop_v1",
        "status": "ok",
        "action": "recent-unlocks",
        "items": unlocked,
        "snapshotPath": snapshot_path.to_string_lossy().to_string()
    })
}

fn dashboard_achievements_session_badges(store: &AppStore, session_id: &str) -> Value {
    let (_, snapshot_path, _) = dashboard_achievements_paths(store);
    let sessions = read_json_file(&snapshot_path)
        .and_then(|snapshot| snapshot.get("sessions").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let session = sessions.into_iter().find(|session| {
        session
            .get("session_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == session_id)
    });
    let badges = session
        .as_ref()
        .map(|session| {
            let aggregate = aggregate_single_session_stats(session);
            achievement_evaluations(&aggregate)
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|item| {
                    item.get("unlocked")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "schema": "hermes_dashboard_plugins_desktop_v1",
        "status": "ok",
        "action": "session-badges",
        "sessionId": session_id,
        "badges": badges,
        "snapshotPath": snapshot_path.to_string_lossy().to_string()
    })
}

fn scan_synthchat_achievement_history(store: &AppStore) -> AppResult<Value> {
    let conversations = store.conversations()?;
    let runs = store.agent_runs()?;
    let traces = store.tool_traces().unwrap_or_default();
    let mut runs_by_conversation: HashMap<String, Vec<AgentRunRecord>> = HashMap::new();
    let mut run_conversation: HashMap<String, String> = HashMap::new();
    for run in runs {
        run_conversation.insert(run.run_id.clone(), run.conversation_id.clone());
        runs_by_conversation
            .entry(run.conversation_id.clone())
            .or_default()
            .push(run);
    }
    let mut traces_by_conversation: HashMap<String, Vec<ToolTraceEntry>> = HashMap::new();
    for trace in traces {
        if let Some(conversation_id) = trace
            .event
            .run_id
            .as_ref()
            .and_then(|run_id| run_conversation.get(run_id))
        {
            traces_by_conversation
                .entry(conversation_id.clone())
                .or_default()
                .push(trace);
        }
    }

    let mut session_values = Vec::new();
    let mut checkpoint_sessions = serde_json::Map::new();
    for conversation in conversations {
        let messages = store.messages(&conversation.id, None).unwrap_or_default();
        let runs = runs_by_conversation
            .remove(&conversation.id)
            .unwrap_or_default();
        let traces = traces_by_conversation
            .remove(&conversation.id)
            .unwrap_or_default();
        let stats = analyze_synthchat_session(&conversation, &messages, &runs, &traces);
        checkpoint_sessions.insert(
            conversation.id.clone(),
            json!({
                "fingerprint": {
                    "started_at": conversation.created_at,
                    "last_active": conversation.updated_at,
                    "title": conversation.title,
                    "message_count": messages.len(),
                    "run_count": runs.len(),
                    "tool_trace_count": traces.len()
                },
                "stats": stats
            }),
        );
        session_values.push(stats);
    }
    let aggregate = aggregate_sessions(&session_values);
    let total = session_values.len();
    Ok(json!({
        "sessions": session_values,
        "aggregate": aggregate,
        "checkpoint_sessions": checkpoint_sessions,
        "scan_meta": {
            "mode": "desktop_native",
            "sessions_total": total,
            "sessions_rescanned": total,
            "sessions_reused": 0,
            "sessions_scanned_so_far": total,
            "sessions_expected_total": total,
            "source": "SynthChat AppStore"
        }
    }))
}

fn analyze_synthchat_session(
    conversation: &Conversation,
    messages: &[ChatMessage],
    runs: &[AgentRunRecord],
    traces: &[ToolTraceEntry],
) -> Value {
    let mut tool_names = BTreeSet::new();
    let mut tool_sequence = Vec::new();
    let mut text_parts = Vec::new();
    let mut model_names = BTreeSet::new();

    for message in messages {
        text_parts.push(message.content.clone());
        collect_model_names(&message.provider_data, &mut model_names);
        collect_tool_names_from_value(&message.provider_data, &mut tool_names, &mut tool_sequence);
    }
    for run in runs {
        if !run.user_request.is_empty() {
            text_parts.push(run.user_request.clone());
        }
        if let Some(error) = &run.error {
            text_parts.push(error.clone());
        }
        for event in &run.tool_events {
            collect_tool_names_from_value(
                &Some(event.clone()),
                &mut tool_names,
                &mut tool_sequence,
            );
            text_parts.push(event.to_string());
        }
    }
    for trace in traces {
        tool_names.insert(trace.tool_name.clone());
        tool_sequence.push(trace.tool_name.clone());
        text_parts.push(trace.event.summary.clone());
        if let Some(error) = &trace.error {
            text_parts.push(error.clone());
        }
        if let Some(error) = &trace.event.error {
            text_parts.push(error.clone());
        }
    }

    let full_text = text_parts.join("\n");
    let lower = full_text.to_ascii_lowercase();
    let terminal_calls = count_tool(&tool_sequence, &["terminal"]);
    let web_calls = count_tool(&tool_sequence, &["web_search", "web_extract"]);
    let web_extract_calls = count_tool(&tool_sequence, &["web_extract"]);
    let browser_calls = count_tool(&tool_sequence, &["browser"]);
    let patch_calls = count_tool(&tool_sequence, &["patch"]);
    let file_reads_searches = count_tool(&tool_sequence, &["read_file", "search_files"]);
    let file_tool_calls = count_tool(
        &tool_sequence,
        &["read_file", "write_file", "patch", "search_files"],
    );
    let delegate_calls = count_tool(&tool_sequence, &["delegate_task"]);
    let process_calls =
        count_tool(&tool_sequence, &["process"]) + lower.matches("background").count();
    let cron_calls = count_tool(&tool_sequence, &["cronjob"]);
    let image_vision_calls = count_tool(&tool_sequence, &["image", "vision"]);
    let tts_calls = count_tool(&tool_sequence, &["tts", "text_to_speech"]);
    let error_count = count_any(
        &lower,
        &["error", "failed", "failure", "traceback", "exception"],
    );
    let files_touched_count = count_file_like_mentions(&full_text);

    let mut out = serde_json::Map::new();
    out.insert("session_id".into(), json!(conversation.id));
    out.insert(
        "title".into(),
        json!(if conversation.title.trim().is_empty() {
            "Untitled session"
        } else {
            conversation.title.as_str()
        }),
    );
    out.insert("started_at".into(), json!(conversation.created_at));
    out.insert("last_active".into(), json!(conversation.updated_at));
    out.insert("message_count".into(), json!(messages.len()));
    out.insert("run_count".into(), json!(runs.len()));
    out.insert("tool_call_count".into(), json!(tool_sequence.len()));
    out.insert("tool_names".into(), json!(tool_names));
    out.insert("distinct_tool_count".into(), json!(tool_names.len()));
    out.insert("error_count".into(), json!(error_count));
    for (key, value) in [
        ("terminal_calls", terminal_calls),
        ("web_calls", web_calls),
        ("web_extract_calls", web_extract_calls),
        ("browser_calls", browser_calls),
        ("web_browser_calls", web_calls + browser_calls),
        ("patch_calls", patch_calls),
        ("file_reads_searches", file_reads_searches),
        ("file_tool_calls", file_tool_calls),
        ("files_touched_count", files_touched_count),
        ("delegate_calls", delegate_calls),
        ("process_calls", process_calls),
        ("cron_calls", cron_calls),
        ("image_vision_calls", image_vision_calls),
        ("tts_calls", tts_calls),
    ] {
        out.insert(key.into(), json!(value));
    }
    let metric_values = [
        (
            "traceback_events",
            count_any(&lower, &["traceback", "exception"]),
        ),
        (
            "log_read_events",
            count_any(
                &lower,
                &[
                    "gateway.log",
                    "errors.log",
                    "agent.log",
                    " logs",
                    "/api/logs",
                ],
            ),
        ),
        (
            "port_conflict_events",
            usize::from(contains_any(
                &lower,
                &["eaddrinuse", "already in use", "port 3000", "port 5173"],
            )),
        ),
        (
            "permission_denied_events",
            count_any(
                &lower,
                &["permission denied", "eacces", "operation not permitted"],
            ),
        ),
        (
            "install_error_events",
            usize::from(
                contains_any(
                    &lower,
                    &["npm install", "pnpm install", "pip install", "uv add"],
                ) && error_count > 0,
            ),
        ),
        (
            "install_success_events",
            usize::from(
                contains_any(
                    &lower,
                    &["npm install", "pnpm install", "pip install", "uv add"],
                ) && contains_any(&lower, &["success", "passed", "ok", "done"]),
            ),
        ),
        (
            "restart_after_error_events",
            usize::from(
                error_count > 0
                    && contains_any(&lower, &["restart", "reload", " kill ", " start "]),
            ),
        ),
        (
            "env_var_error_events",
            count_any(
                &lower,
                &[
                    "missing env",
                    "api key",
                    "environment variable",
                    "not configured",
                    "unauthorized",
                    "auth",
                ],
            ),
        ),
        (
            "yaml_error_events",
            if error_count > 0 {
                count_any(&lower, &["yaml", "yml", "parse error"])
            } else {
                0
            },
        ),
        (
            "docker_conflict_events",
            count_any(&lower, &["docker", "container name conflict"]),
        ),
        (
            "frontend_activity_events",
            count_any(
                &lower,
                &[
                    ".css", ".svg", ".tsx", ".jsx", "frontend", "tailwind", "react",
                ],
            ),
        ),
        (
            "css_activity_events",
            count_any(
                &lower,
                &[".css", "tailwind", "style", "classname", "visual"],
            ),
        ),
        (
            "git_events",
            count_any(
                &lower,
                &[
                    "git commit",
                    "git push",
                    "git merge",
                    "git rebase",
                    "git status",
                    "git diff",
                ],
            ),
        ),
        (
            "tiny_patch_after_errors_events",
            usize::from(
                error_count >= 5
                    && contains_any(&lower, &["one character", "single character", "typo"]),
            ),
        ),
        (
            "skill_events",
            count_tool(&tool_sequence, &["skill"]) + count_any(&lower, &["skill"]),
        ),
        (
            "skill_manage_events",
            count_tool(&tool_sequence, &["skill_manage"]),
        ),
        (
            "memory_events",
            count_tool(&tool_sequence, &["memory", "mnemosyne"]),
        ),
        (
            "memory_write_events",
            count_tool(
                &tool_sequence,
                &["remember_fact", "memory", "mnemosyne_remember"],
            ),
        ),
        (
            "context_events",
            count_any(&lower, &["compress", "context window", "token", "cache"]),
        ),
        (
            "gateway_events",
            count_any(
                &lower,
                &["gateway", "discord", "telegram", "slack", "api_server"],
            ),
        ),
        (
            "plugin_events",
            count_any(&lower, &["plugin", "dashboard_plugins", "manifest.json"]),
        ),
        (
            "rollback_events",
            count_any(&lower, &["rollback", "checkpoint"]),
        ),
        (
            "docs_activity_events",
            count_any(&lower, &["docs", "documentation", "readme"]),
        ),
        (
            "model_events",
            count_any(
                &lower,
                &[
                    "model",
                    "provider",
                    "openrouter",
                    "codex",
                    "gemini",
                    "claude",
                    "anthropic",
                    "openai",
                    "mistral",
                    "qwen",
                    "deepseek",
                    "llama",
                    "ollama",
                ],
            ),
        ),
        ("openrouter_events", count_any(&lower, &["openrouter"])),
        ("codex_events", count_any(&lower, &["codex"])),
        ("claude_events", count_any(&lower, &["claude", "anthropic"])),
        (
            "gemini_events",
            count_any(&lower, &["gemini", "google ai", "google model"]),
        ),
        (
            "local_model_events",
            count_any(
                &lower,
                &[
                    "ollama",
                    "llama.cpp",
                    "gguf",
                    "vllm",
                    "local model",
                    "open-weight",
                    "open weights",
                ],
            ),
        ),
        (
            "toolset_events",
            count_any(
                &lower,
                &[
                    "toolset",
                    "enabled_toolsets",
                    "browser tool",
                    "terminal tool",
                    "file tool",
                    "web tool",
                ],
            ),
        ),
        (
            "config_events",
            count_any(
                &lower,
                &[
                    "config.yaml",
                    "config.yml",
                    ".env",
                    "manifest.json",
                    "settings.json",
                    "pyproject.toml",
                    "package.json",
                ],
            ),
        ),
        (
            "git_history_events",
            count_any(
                &lower,
                &[
                    "git rebase",
                    "git merge",
                    "git fetch",
                    "git pull",
                    "git push",
                    "git tag",
                    "merge conflict",
                ],
            ),
        ),
        (
            "test_events",
            count_any(
                &lower,
                &[
                    "pytest",
                    "unittest",
                    "vitest",
                    "playwright",
                    "npm test",
                    "cargo test",
                    "tests passed",
                    "cargo check",
                ],
            ),
        ),
        (
            "screenshot_events",
            count_any(
                &lower,
                &[
                    "screenshot",
                    "playwright",
                    "vision_analyze",
                    "browser_vision",
                    ".png",
                ],
            ),
        ),
        (
            "release_events",
            count_any(
                &lower,
                &["git tag", "release", "version bump", "changelog", "publish"],
            ),
        ),
        (
            "cache_events",
            count_any(&lower, &["cache hit", "prompt caching", "cache_read"]),
        ),
    ];
    for (key, value) in metric_values {
        out.insert(key.into(), json!(value));
    }
    out.insert("model_names".into(), json!(model_names));
    Value::Object(out)
}

fn aggregate_sessions(sessions: &[Value]) -> Value {
    let mut agg = BTreeMap::<String, i64>::new();
    for key in [
        "session_count",
        "max_tool_calls_in_session",
        "max_distinct_tools_in_session",
        "max_messages_in_session",
        "max_terminal_calls_in_session",
        "max_file_tool_calls_in_session",
        "max_web_calls_in_session",
        "max_web_browser_calls_in_session",
        "max_files_touched_in_session",
        "total_errors",
        "total_tool_calls",
        "total_terminal_calls",
        "total_web_calls",
        "total_web_extract_calls",
        "total_patch_calls",
        "total_file_reads_searches",
        "total_delegate_calls",
        "total_process_calls",
        "total_cron_calls",
        "browser_calls",
        "image_vision_calls",
        "tts_calls",
        "distinct_model_count",
        "distinct_provider_count",
        "local_model_chat_sessions",
        "weekend_sessions",
        "night_sessions",
    ] {
        agg.insert(key.into(), 0);
    }
    let sum_keys = [
        "traceback_events",
        "log_read_events",
        "port_conflict_events",
        "permission_denied_events",
        "install_error_events",
        "install_success_events",
        "restart_after_error_events",
        "env_var_error_events",
        "yaml_error_events",
        "docker_conflict_events",
        "frontend_activity_events",
        "css_activity_events",
        "git_events",
        "tiny_patch_after_errors_events",
        "skill_events",
        "skill_manage_events",
        "memory_events",
        "memory_write_events",
        "context_events",
        "gateway_events",
        "plugin_events",
        "rollback_events",
        "docs_activity_events",
        "model_events",
        "openrouter_events",
        "codex_events",
        "claude_events",
        "gemini_events",
        "local_model_events",
        "toolset_events",
        "config_events",
        "git_history_events",
        "test_events",
        "screenshot_events",
        "release_events",
        "cache_events",
    ];
    for key in sum_keys {
        agg.insert(key.into(), 0);
    }

    let mut model_names = BTreeSet::new();
    let mut provider_names = BTreeSet::new();
    agg.insert("session_count".into(), sessions.len() as i64);
    for session in sessions {
        max_metric(
            &mut agg,
            "max_tool_calls_in_session",
            session,
            "tool_call_count",
        );
        max_metric(
            &mut agg,
            "max_distinct_tools_in_session",
            session,
            "distinct_tool_count",
        );
        max_metric(
            &mut agg,
            "max_messages_in_session",
            session,
            "message_count",
        );
        max_metric(
            &mut agg,
            "max_terminal_calls_in_session",
            session,
            "terminal_calls",
        );
        max_metric(
            &mut agg,
            "max_file_tool_calls_in_session",
            session,
            "file_tool_calls",
        );
        max_metric(&mut agg, "max_web_calls_in_session", session, "web_calls");
        max_metric(
            &mut agg,
            "max_web_browser_calls_in_session",
            session,
            "web_browser_calls",
        );
        max_metric(
            &mut agg,
            "max_files_touched_in_session",
            session,
            "files_touched_count",
        );
        add_metric(&mut agg, "total_errors", session, "error_count");
        add_metric(&mut agg, "total_tool_calls", session, "tool_call_count");
        add_metric(&mut agg, "total_terminal_calls", session, "terminal_calls");
        add_metric(&mut agg, "total_web_calls", session, "web_calls");
        add_metric(
            &mut agg,
            "total_web_extract_calls",
            session,
            "web_extract_calls",
        );
        add_metric(&mut agg, "total_patch_calls", session, "patch_calls");
        add_metric(
            &mut agg,
            "total_file_reads_searches",
            session,
            "file_reads_searches",
        );
        add_metric(&mut agg, "total_delegate_calls", session, "delegate_calls");
        add_metric(&mut agg, "total_process_calls", session, "process_calls");
        add_metric(&mut agg, "total_cron_calls", session, "cron_calls");
        add_metric(&mut agg, "browser_calls", session, "browser_calls");
        add_metric(
            &mut agg,
            "image_vision_calls",
            session,
            "image_vision_calls",
        );
        add_metric(&mut agg, "tts_calls", session, "tts_calls");
        for key in sum_keys {
            add_metric(&mut agg, key, session, key);
        }
        if let Some(models) = session.get("model_names").and_then(Value::as_array) {
            let mut local = false;
            for model in models.iter().filter_map(Value::as_str) {
                if !model.trim().is_empty() && model != "None" {
                    model_names.insert(model.to_string());
                }
                if let Some(provider) = model_provider(model) {
                    provider_names.insert(provider);
                }
                if is_local_model_name(model) {
                    local = true;
                }
            }
            if local {
                *agg.entry("local_model_chat_sessions".into()).or_default() += 1;
            }
        }
        if let Some(started) = session.get("started_at").and_then(Value::as_str) {
            if let Ok(dt) = DateTime::parse_from_rfc3339(started) {
                let dt = dt.with_timezone(&Utc);
                if dt.weekday().number_from_monday() >= 6 {
                    *agg.entry("weekend_sessions".into()).or_default() += 1;
                }
                if dt.hour() < 6 || dt.hour() >= 23 {
                    *agg.entry("night_sessions".into()).or_default() += 1;
                }
            }
        }
    }
    agg.insert("distinct_model_count".into(), model_names.len() as i64);
    agg.insert(
        "distinct_provider_count".into(),
        provider_names.len() as i64,
    );
    json!(agg)
}

fn aggregate_single_session_stats(session: &Value) -> Value {
    aggregate_sessions(&[session.clone()])
}

fn achievement_evaluations(aggregate: &Value) -> Value {
    let defs = [
        (
            "let_him_cook",
            "Let Him Cook",
            "Agent Autonomy",
            "max_tool_calls_in_session",
            200,
        ),
        (
            "autonomous_avalanche",
            "Autonomous Avalanche",
            "Agent Autonomy",
            "total_tool_calls",
            1000,
        ),
        (
            "toolchain_maxxer",
            "Toolchain Maxxer",
            "Agent Autonomy",
            "max_distinct_tools_in_session",
            18,
        ),
        (
            "subagent_commander",
            "Subagent Commander",
            "Agent Autonomy",
            "total_delegate_calls",
            5,
        ),
        (
            "background_process_enjoyer",
            "Background Process Enjoyer",
            "Agent Autonomy",
            "total_process_calls",
            300,
        ),
        (
            "cron_necromancer",
            "Cron Necromancer",
            "Agent Autonomy",
            "total_cron_calls",
            1000,
        ),
        (
            "red_text_connoisseur",
            "Red Text Connoisseur",
            "Debugging Chaos",
            "total_errors",
            1500,
        ),
        (
            "stack_trace_sommelier",
            "Stack Trace Sommelier",
            "Debugging Chaos",
            "traceback_events",
            300,
        ),
        (
            "actually_read_the_logs",
            "Actually Read The Logs",
            "Debugging Chaos",
            "log_read_events",
            1000,
        ),
        (
            "port_3000_taken",
            "Port 3000 Is Taken",
            "Debugging Chaos",
            "port_conflict_events",
            15,
        ),
        (
            "permission_denied_any_percent",
            "Permission Denied Any%",
            "Debugging Chaos",
            "permission_denied_events",
            25,
        ),
        (
            "forgot_the_env_var",
            "Forgot The Env Var",
            "Debugging Chaos",
            "env_var_error_events",
            5000,
        ),
        (
            "yaml_colon_incident",
            "YAML Colon Incident",
            "Debugging Chaos",
            "yaml_error_events",
            1000,
        ),
        (
            "docker_name_collision",
            "Docker Name Collision",
            "Debugging Chaos",
            "docker_conflict_events",
            75,
        ),
        (
            "supposed_to_be_quick",
            "This Was Supposed To Be Quick",
            "Vibe Coding",
            "max_messages_in_session",
            300,
        ),
        (
            "one_more_small_change",
            "One More Small Change",
            "Vibe Coding",
            "max_file_tool_calls_in_session",
            150,
        ),
        (
            "vibe_architect",
            "Vibe Architect",
            "Vibe Coding",
            "max_files_touched_in_session",
            300,
        ),
        (
            "pixel_goblin",
            "Pixel Goblin",
            "Vibe Coding",
            "frontend_activity_events",
            20000,
        ),
        (
            "css_exorcist",
            "CSS Exorcist",
            "Vibe Coding",
            "css_activity_events",
            10000,
        ),
        (
            "skillsmith",
            "Skillsmith",
            "Hermes Native",
            "skill_events",
            5000,
        ),
        (
            "skill_issue_skill_created",
            "Skill Issue? Skill Created.",
            "Hermes Native",
            "skill_manage_events",
            25,
        ),
        (
            "memory_keeper",
            "Memory Keeper",
            "Hermes Native",
            "memory_events",
            100,
        ),
        (
            "memory_palace",
            "Memory Palace",
            "Hermes Native",
            "memory_write_events",
            100,
        ),
        (
            "context_dragon",
            "Context Dragon",
            "Hermes Native",
            "context_events",
            5000,
        ),
        (
            "gateway_dweller",
            "Gateway Dweller",
            "Hermes Native",
            "gateway_events",
            5000,
        ),
        (
            "plugin_goblin",
            "Plugin Goblin",
            "Hermes Native",
            "plugin_events",
            1000,
        ),
        (
            "rollback_wizard",
            "Rollback Wizard",
            "Hermes Native",
            "rollback_events",
            500,
        ),
        (
            "rabbit_hole_certified",
            "Rabbit Hole Certified",
            "Research/Web",
            "total_web_calls",
            400,
        ),
        (
            "citation_goblin",
            "Citation Goblin",
            "Research/Web",
            "total_web_extract_calls",
            100,
        ),
        (
            "docs_archaeologist",
            "Docs Archaeologist",
            "Research/Web",
            "docs_activity_events",
            5000,
        ),
        (
            "browser_possession",
            "Browser Possession",
            "Research/Web",
            "browser_calls",
            75,
        ),
        (
            "terminal_goblin",
            "Terminal Goblin",
            "Tool Mastery",
            "total_terminal_calls",
            750,
        ),
        (
            "patch_wizard",
            "Patch Wizard",
            "Tool Mastery",
            "total_patch_calls",
            250,
        ),
        (
            "file_archaeologist",
            "File Archaeologist",
            "Tool Mastery",
            "total_file_reads_searches",
            750,
        ),
        (
            "image_whisperer",
            "Image Whisperer",
            "Tool Mastery",
            "image_vision_calls",
            100,
        ),
        (
            "voice_of_the_machine",
            "Voice Of The Machine",
            "Tool Mastery",
            "tts_calls",
            10,
        ),
        (
            "model_hopper",
            "Model Hopper",
            "Model Lore",
            "model_events",
            10000,
        ),
        (
            "openrouter_enjoyer",
            "OpenRouter Enjoyer",
            "Model Lore",
            "openrouter_events",
            250,
        ),
        (
            "codex_conjurer",
            "Codex Conjurer",
            "Model Lore",
            "codex_events",
            500,
        ),
        (
            "multi_model_mage",
            "Multi-Model Mage",
            "Model Lore",
            "distinct_model_count",
            10,
        ),
        (
            "five_model_flight",
            "Five-Model Flight",
            "Model Lore",
            "distinct_model_count",
            5,
        ),
        (
            "provider_polyglot",
            "Provider Polyglot",
            "Model Lore",
            "distinct_provider_count",
            2,
        ),
        (
            "model_sommelier",
            "Model Sommelier",
            "Model Lore",
            "model_events",
            250,
        ),
        (
            "claude_confidant",
            "Claude Confidant",
            "Model Lore",
            "claude_events",
            50,
        ),
        (
            "gemini_cartographer",
            "Gemini Cartographer",
            "Model Lore",
            "gemini_events",
            50,
        ),
        (
            "open_weights_pilgrim",
            "Open Weights Pilgrim",
            "Model Lore",
            "local_model_chat_sessions",
            1,
        ),
        (
            "toolset_cartographer",
            "Toolset Cartographer",
            "Hermes Native",
            "toolset_events",
            20,
        ),
        (
            "config_surgeon",
            "Config Surgeon",
            "Hermes Native",
            "config_events",
            100,
        ),
        (
            "rebase_acrobat",
            "Rebase Acrobat",
            "Vibe Coding",
            "git_history_events",
            10,
        ),
        (
            "test_suite_tamer",
            "Test Suite Tamer",
            "Tool Mastery",
            "test_events",
            100,
        ),
        (
            "screenshot_hunter",
            "Screenshot Hunter",
            "Tool Mastery",
            "screenshot_events",
            50,
        ),
        (
            "marathon_operator",
            "Marathon Operator",
            "Lifestyle",
            "session_count",
            75,
        ),
        (
            "weekend_warrior",
            "Weekend Warrior",
            "Lifestyle",
            "weekend_sessions",
            25,
        ),
        (
            "night_shift_operator",
            "Night Shift Operator",
            "Lifestyle",
            "night_sessions",
            25,
        ),
        (
            "cache_hit_appreciator",
            "Cache Hit Appreciator",
            "Lifestyle",
            "cache_events",
            100,
        ),
    ];
    let mut items = defs
        .iter()
        .map(|(id, name, category, metric, threshold)| {
            let progress = aggregate.get(*metric).and_then(Value::as_i64).unwrap_or(0);
            let unlocked = progress >= *threshold;
            json!({
                "id": id,
                "name": name,
                "category": category,
                "kind": "tiered",
                "threshold_metric": metric,
                "unlocked": unlocked,
                "discovered": progress > 0,
                "state": if unlocked { "unlocked" } else if progress > 0 { "discovered" } else { "secret" },
                "tier": if unlocked { json!("Copper") } else { Value::Null },
                "progress": progress,
                "next_tier": if unlocked { Value::Null } else { json!("Copper") },
                "next_threshold": threshold,
                "progress_pct": if unlocked { 100 } else { ((progress * 100) / (*threshold as i64).max(1)).min(99) },
                "criteria": format!("Requirement: {} >= {}.", metric, threshold),
                "evidence": if unlocked { best_session_evidence(metric, aggregate) } else { Value::Null }
            })
        })
        .collect::<Vec<_>>();
    items.extend([
        multi_condition_achievement(
            "full_send",
            "Full Send",
            "Agent Autonomy",
            &[
                ("max_terminal_calls_in_session", 180),
                ("max_file_tool_calls_in_session", 120),
                ("max_web_browser_calls_in_session", 60),
            ],
            aggregate,
        ),
        multi_condition_achievement(
            "dependency_hell_tourist",
            "Dependency Hell Tourist",
            "Debugging Chaos",
            &[("install_error_events", 25), ("install_success_events", 10)],
            aggregate,
        ),
        multi_condition_achievement(
            "the_fix_was_restarting",
            "The Fix Was Restarting It",
            "Debugging Chaos",
            &[("restart_after_error_events", 50), ("total_errors", 4000)],
            aggregate,
        ),
        multi_condition_achievement(
            "ship_first_ask_later",
            "Ship First, Ask Later",
            "Vibe Coding",
            &[("git_events", 50), ("max_tool_calls_in_session", 500)],
            aggregate,
        ),
        multi_condition_achievement(
            "one_character_fix",
            "One Character Fix",
            "Vibe Coding",
            &[
                ("tiny_patch_after_errors_events", 5),
                ("total_errors", 4000),
            ],
            aggregate,
        ),
    ]);
    debug_assert_eq!(items.len(), ACHIEVEMENT_COUNT as usize);
    json!(items)
}

fn multi_condition_achievement(
    id: &str,
    name: &str,
    category: &str,
    requirements: &[(&str, i64)],
    aggregate: &Value,
) -> Value {
    let values = requirements
        .iter()
        .map(|(metric, threshold)| {
            let value = aggregate.get(*metric).and_then(Value::as_i64).unwrap_or(0);
            json!({
                "metric": metric,
                "value": value,
                "gte": threshold,
                "complete": value >= *threshold
            })
        })
        .collect::<Vec<_>>();
    let complete = values.iter().all(|item| {
        item.get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    let any_progress = values.iter().any(|item| {
        item.get("value")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            > 0
    });
    let progress_pct = if requirements.is_empty() {
        0
    } else {
        let sum = requirements
            .iter()
            .map(|(metric, threshold)| {
                let value = aggregate.get(*metric).and_then(Value::as_i64).unwrap_or(0);
                ((value * 100) / (*threshold).max(1)).min(100)
            })
            .sum::<i64>();
        (sum / requirements.len() as i64).min(if complete { 100 } else { 99 })
    };
    let criteria = requirements
        .iter()
        .map(|(metric, threshold)| format!("{metric} >= {threshold}"))
        .collect::<Vec<_>>()
        .join("; ");
    json!({
        "id": id,
        "name": name,
        "category": category,
        "kind": "multi_condition",
        "requirements": values,
        "unlocked": complete,
        "discovered": any_progress,
        "state": if complete { "unlocked" } else if any_progress { "discovered" } else { "secret" },
        "tier": Value::Null,
        "progress": progress_pct,
        "next_tier": Value::Null,
        "next_threshold": 100,
        "progress_pct": progress_pct,
        "criteria": format!("Requirement: {criteria}.")
    })
}

fn best_session_evidence(_metric: &str, _aggregate: &Value) -> Value {
    Value::Null
}

fn collect_tool_names_from_value(
    value: &Option<Value>,
    tool_names: &mut BTreeSet<String>,
    tool_sequence: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::Object(map) => {
            for key in ["toolName", "tool_name", "name"] {
                if let Some(name) = map.get(key).and_then(Value::as_str) {
                    if looks_like_tool_name(name) {
                        tool_names.insert(name.to_string());
                        tool_sequence.push(name.to_string());
                    }
                }
            }
            if let Some(calls) = map
                .get("toolCalls")
                .or_else(|| map.get("tool_calls"))
                .and_then(Value::as_array)
            {
                for call in calls {
                    collect_tool_names_from_value(&Some(call.clone()), tool_names, tool_sequence);
                }
            }
            if let Some(function) = map.get("function") {
                collect_tool_names_from_value(&Some(function.clone()), tool_names, tool_sequence);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_tool_names_from_value(&Some(value.clone()), tool_names, tool_sequence);
            }
        }
        _ => {}
    }
}

fn collect_model_names(value: &Option<Value>, model_names: &mut BTreeSet<String>) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::Object(map) => {
            for key in ["model", "modelName", "model_name"] {
                if let Some(model) = map.get(key).and_then(Value::as_str) {
                    model_names.insert(model.to_string());
                }
            }
            for value in map.values() {
                collect_model_names(&Some(value.clone()), model_names);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_model_names(&Some(value.clone()), model_names);
            }
        }
        _ => {}
    }
}

fn looks_like_tool_name(value: &str) -> bool {
    let clean = value.trim();
    !clean.is_empty()
        && clean.len() <= 80
        && clean
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

fn count_tool(tool_sequence: &[String], needles: &[&str]) -> usize {
    tool_sequence
        .iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            needles.iter().any(|needle| lower.contains(needle))
        })
        .count()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn count_any(text: &str, needles: &[&str]) -> usize {
    needles
        .iter()
        .map(|needle| text.matches(needle).count())
        .sum()
}

fn count_file_like_mentions(text: &str) -> usize {
    text.split_whitespace()
        .map(|part| {
            part.trim_matches(|ch: char| {
                ch == '"' || ch == '\'' || ch == ',' || ch == ';' || ch == ')' || ch == '('
            })
        })
        .filter(|part| {
            [
                ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".css", ".html", ".md", ".json",
                ".yaml", ".yml", ".toml", ".sh",
            ]
            .iter()
            .any(|suffix| part.ends_with(suffix))
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn add_metric(agg: &mut BTreeMap<String, i64>, target: &str, session: &Value, source: &str) {
    *agg.entry(target.into()).or_default() +=
        session.get(source).and_then(Value::as_i64).unwrap_or(0);
}

fn max_metric(agg: &mut BTreeMap<String, i64>, target: &str, session: &Value, source: &str) {
    let value = session.get(source).and_then(Value::as_i64).unwrap_or(0);
    let entry = agg.entry(target.into()).or_default();
    *entry = (*entry).max(value);
}

fn model_provider(model_name: &str) -> Option<String> {
    let name = model_name.trim().to_ascii_lowercase();
    if name.is_empty() || name == "none" {
        return None;
    }
    if let Some((provider, _)) = name.split_once('/') {
        return Some(provider.to_string());
    }
    for provider in [
        "openai",
        "anthropic",
        "google",
        "gemini",
        "mistral",
        "meta",
        "qwen",
        "deepseek",
        "xai",
        "nous",
        "ollama",
        "groq",
        "openrouter",
        "codex",
    ] {
        if name.contains(provider) {
            return Some(
                if provider == "gemini" {
                    "google"
                } else {
                    provider
                }
                .into(),
            );
        }
    }
    name.split(':')
        .next()
        .and_then(|part| part.split('-').next())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
}

fn is_local_model_name(model_name: &str) -> bool {
    let name = model_name.trim().to_ascii_lowercase();
    [
        "ollama",
        "llama.cpp",
        "localhost",
        "127.0.0.1",
        "local/",
        "local:",
        "gguf",
        "vllm-local",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}
