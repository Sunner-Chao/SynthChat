use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{AgentDefinition, McpServer},
    store::AppStore,
};

use super::{
    delegation_request::DelegateTaskRequest, normalize_mcp_server_toolset_component,
    normalize_toolset_name,
};

pub(super) fn delegation_child_toolsets(
    agent: &AgentDefinition,
    request: &DelegateTaskRequest,
    inherit_mcp_toolsets: bool,
) -> Option<Vec<String>> {
    if request.toolsets.is_empty() {
        return None;
    }
    let mut toolsets = request.toolsets.clone();
    if request.toolsets.iter().any(|toolset| {
        normalize_toolset_name(toolset) == "browser"
            && !toolset.trim().to_ascii_lowercase().starts_with("tool:")
    }) {
        push_unique_toolset(&mut toolsets, "browser_safe");
        for unsafe_tool in [
            "browser_click",
            "browser_type",
            "browser_press",
            "browser_scroll",
            "browser_cdp",
            "browser_dialog",
            "browser_record",
            "browser_vision",
            "browser_console",
        ] {
            push_unique_toolset(&mut toolsets, &format!("not_tool:{unsafe_tool}"));
        }
    }
    if inherit_mcp_toolsets && agent.mcp_enabled {
        push_unique_toolset(&mut toolsets, "mcp");
        push_unique_toolset(&mut toolsets, "mcp_utility");
        for server_id in &agent.enabled_mcp_servers {
            let server_toolset = format!(
                "server:{}",
                normalize_mcp_server_toolset_component(server_id)
            );
            push_unique_toolset(&mut toolsets, &server_toolset);
        }
        for parent_toolset in &agent.enabled_toolsets {
            let normalized = normalize_toolset_name(parent_toolset);
            if normalized == "mcp"
                || normalized == "mcp_utility"
                || normalized.starts_with("server:")
            {
                if let Some(server) = normalized.strip_prefix("server:") {
                    let server_toolset =
                        format!("server:{}", normalize_mcp_server_toolset_component(server));
                    push_unique_toolset(&mut toolsets, &server_toolset);
                } else {
                    push_unique_toolset(&mut toolsets, parent_toolset);
                }
            }
        }
    }
    Some(toolsets)
}

pub(super) fn acp_mcp_servers_for_agent(
    store: &AppStore,
    agent: &AgentDefinition,
    request: &DelegateTaskRequest,
    inherit_mcp_toolsets: bool,
) -> AppResult<Vec<Value>> {
    if !agent.mcp_enabled || (!inherit_mcp_toolsets && !delegate_request_wants_mcp(request)) {
        return Ok(Vec::new());
    }
    let enabled_ids = agent
        .enabled_mcp_servers
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    let mut servers = Vec::new();
    for value in store.static_list("mcpServers")? {
        let server: McpServer = serde_json::from_value(value)?;
        if !server.enabled {
            continue;
        }
        if !enabled_ids.is_empty()
            && !enabled_ids.contains(&server.id)
            && !enabled_ids.contains(&server.name)
        {
            continue;
        }
        if let Some(acp_server) = acp_mcp_server_payload(&server) {
            servers.push(acp_server);
        }
    }
    Ok(servers)
}

fn delegate_request_wants_mcp(request: &DelegateTaskRequest) -> bool {
    request.toolsets.iter().any(|toolset| {
        let normalized = normalize_toolset_name(toolset);
        normalized == "mcp" || normalized == "mcp_utility" || normalized.starts_with("server:")
    })
}

fn acp_mcp_server_payload(server: &McpServer) -> Option<Value> {
    let name = if server.name.trim().is_empty() {
        server.id.trim()
    } else {
        server.name.trim()
    };
    if name.is_empty() {
        return None;
    }
    let url = server.url.as_deref().map(str::trim).unwrap_or("");
    if !url.is_empty() {
        return Some(json!({
            "name": name,
            "url": url,
            "headers": []
        }));
    }
    let command = server.command.trim();
    if command.is_empty() {
        return None;
    }
    let env = server
        .env
        .as_ref()
        .map(|values| {
            values
                .iter()
                .map(|(name, value)| json!({"name": name, "value": value}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(json!({
        "name": name,
        "command": command,
        "args": server.args.clone(),
        "env": env
    }))
}

fn push_unique_toolset(toolsets: &mut Vec<String>, toolset: &str) {
    if !toolsets
        .iter()
        .any(|existing| normalize_toolset_name(existing) == normalize_toolset_name(toolset))
    {
        toolsets.push(toolset.to_string());
    }
}
