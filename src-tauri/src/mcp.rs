use std::{path::Path, process::Stdio, time::Instant};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    time::{timeout, Duration},
};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, tool_event_kind, McpCallResult, McpListToolsResult, McpServer,
        McpToolInfo, ToolDefinition, ToolEvent, ToolTraceEntry,
    },
    store::AppStore,
};

pub async fn list_tools(
    store: &AppStore,
    server_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<McpListToolsResult> {
    let server = get_server(store, &server_id)?;
    let started = Instant::now();
    let timeout_secs = timeout_seconds.unwrap_or(server.timeout_seconds).max(1);
    let result = timeout(Duration::from_secs(timeout_secs), async {
        if server
            .url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            mcp_http_json_rpc_method(&server, "tools/list", json!({})).await
        } else if server.protocol == "oneShotJson" {
            one_shot_json(&server, json!({"method": "tools/list", "params": {}})).await
        } else {
            mcp_json_rpc_tools_list(&server).await
        }
    })
    .await;

    let elapsed_ms = started.elapsed().as_millis();
    let response = match result {
        Ok(Ok(raw)) => {
            let tools = parse_tools(&raw);
            let mut definitions = tools
                .iter()
                .map(|tool| ToolDefinition {
                    name: format!("{}.{}", server.id, tool.name),
                    display_name: tool.name.clone(),
                    description: tool.description.clone().unwrap_or_default(),
                    source: "mcp".into(),
                    server_id: server.id.clone(),
                    tool_name: tool.name.clone(),
                    input_schema: tool
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| json!({"type": "object"})),
                    requires_approval: requires_approval(&tool.name, tool.description.as_deref()),
                })
                .collect::<Vec<_>>();
            definitions.extend(mcp_utility_tool_definitions(&server));
            merge_tool_definitions(store, &server.id, definitions)?;
            McpListToolsResult {
                ok: true,
                timed_out: false,
                elapsed_ms,
                tools,
                raw: Some(raw),
                error: None,
            }
        }
        Ok(Err(error)) => McpListToolsResult {
            ok: false,
            timed_out: false,
            elapsed_ms,
            tools: vec![],
            raw: None,
            error: Some(error.to_string()),
        },
        Err(_) => McpListToolsResult {
            ok: false,
            timed_out: true,
            elapsed_ms,
            tools: vec![],
            raw: None,
            error: Some(format!("timed out after {timeout_secs}s")),
        },
    };
    Ok(response)
}

pub async fn call_tool(
    store: &AppStore,
    server_id: String,
    tool_name: String,
    payload: Value,
    timeout_seconds: Option<u64>,
) -> AppResult<McpCallResult> {
    let server = get_server(store, &server_id)?;
    let started = Instant::now();
    let timeout_secs = timeout_seconds.unwrap_or(server.timeout_seconds).max(1);
    let result = timeout(Duration::from_secs(timeout_secs), async {
        if let Some(request) = mcp_utility_request(&tool_name, payload.clone()) {
            let (method, params) = request?;
            if server
                .url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty())
            {
                mcp_http_json_rpc_method(&server, &method, params).await
            } else if server.protocol == "oneShotJson" {
                one_shot_json(&server, json!({"method": method, "params": params})).await
            } else {
                mcp_json_rpc_method(&server, &method, params).await
            }
        } else if server
            .url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            mcp_http_json_rpc_method(
                &server,
                "tools/call",
                json!({"name": tool_name, "arguments": payload}),
            )
            .await
        } else if server.protocol == "oneShotJson" {
            one_shot_json(&server, json!({"method": tool_name, "params": payload})).await
        } else {
            mcp_json_rpc_call(&server, &tool_name, payload.clone()).await
        }
    })
    .await;
    let elapsed_ms = started.elapsed().as_millis();

    let (ok, timed_out, stdout, stderr, error, raw) = match result {
        Ok(Ok(raw)) => (true, false, raw.to_string(), String::new(), None, Some(raw)),
        Ok(Err(error)) => (
            false,
            false,
            String::new(),
            error.to_string(),
            Some(error.to_string()),
            None,
        ),
        Err(_) => (
            false,
            true,
            String::new(),
            String::new(),
            Some(format!("timed out after {timeout_secs}s")),
            None,
        ),
    };

    let event = ToolEvent {
        status: Some("completed".into()),
        reference_id: None,
        call_id: Some(new_id("call")),
        run_id: None,
        checkpoint_id: None,
        event_type: "mcp_tool".into(),
        server_id: server.id.clone(),
        tool_name: tool_name.clone(),
        ok,
        timed_out,
        elapsed_ms,
        kind: tool_event_kind(&server.id, &tool_name, None),
        title: format!("{} · {}", server.name, tool_name),
        summary: if ok {
            "工具调用完成".into()
        } else {
            error.clone().unwrap_or_else(|| "工具调用失败".into())
        },
        path: None,
        exists: None,
        mime_type: None,
        text: if stdout.is_empty() {
            None
        } else {
            Some(stdout.clone())
        },
        error: error.clone(),
        raw,
    };
    store.append_tool_trace(ToolTraceEntry {
        id: new_id("trace"),
        created_at: now_iso(),
        server_id: server.id,
        tool_name,
        ok,
        timed_out,
        elapsed_ms,
        payload,
        event,
        error: error.clone(),
    })?;

    Ok(McpCallResult {
        ok,
        timed_out,
        elapsed_ms,
        stdout,
        stderr,
        error,
    })
}

pub async fn refresh_tool_registry(store: &AppStore) -> AppResult<Vec<ToolDefinition>> {
    let servers = mcp_servers(store)?;
    let mut definitions = vec![];
    for server in servers.into_iter().filter(|server| server.enabled) {
        if let Ok(result) = list_tools(store, server.id.clone(), Some(server.timeout_seconds)).await
        {
            for tool in result.tools {
                let description = tool.description.clone().unwrap_or_default();
                let requires_approval = requires_approval(&tool.name, Some(&description));
                definitions.push(ToolDefinition {
                    name: format!("{}.{}", server.id, tool.name),
                    display_name: tool.name.clone(),
                    description,
                    source: "mcp".into(),
                    server_id: server.id.clone(),
                    tool_name: tool.name,
                    input_schema: tool
                        .input_schema
                        .unwrap_or_else(|| json!({"type": "object"})),
                    requires_approval,
                });
            }
            definitions.extend(mcp_utility_tool_definitions(&server));
        }
    }
    definitions.extend(capability_tool_definitions(store)?);
    store.set_tool_definitions(definitions)
}

fn capability_tool_definitions(store: &AppStore) -> AppResult<Vec<ToolDefinition>> {
    Ok(store
        .capability_adapters()?
        .into_iter()
        .filter(|adapter| adapter.enabled)
        .map(|adapter| {
            let requires_approval = requires_approval(&adapter.name, Some(&adapter.description));
            ToolDefinition {
                name: adapter.name.clone(),
                display_name: adapter.name,
                description: adapter.description,
                source: "capability".into(),
                server_id: adapter.mcp_server,
                tool_name: adapter.mcp_tool,
                input_schema: adapter.parameters,
                requires_approval,
            }
        })
        .collect())
}

fn mcp_utility_tool_definitions(server: &McpServer) -> Vec<ToolDefinition> {
    let safe_server = sanitize_mcp_name_component(&server.id);
    vec![
        ToolDefinition {
            name: format!("mcp_{safe_server}_list_resources"),
            display_name: "list_resources".into(),
            description: format!("List available resources from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_list_resources".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_read_resource"),
            display_name: "read_resource".into(),
            description: format!("Read a resource by URI from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_read_resource".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"uri": {"type": "string", "description": "URI of the resource to read"}},
                "required": ["uri"]
            }),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_list_prompts"),
            display_name: "list_prompts".into(),
            description: format!("List available prompts from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_list_prompts".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_get_prompt"),
            display_name: "get_prompt".into(),
            description: format!("Get a prompt by name from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_get_prompt".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Name of the prompt to retrieve"},
                    "arguments": {"type": "object", "description": "Optional arguments to pass to the prompt", "additionalProperties": true}
                },
                "required": ["name"]
            }),
            requires_approval: false,
        },
    ]
}

fn mcp_utility_request(tool_name: &str, payload: Value) -> Option<AppResult<(String, Value)>> {
    match tool_name {
        "__mcp_list_resources" => Some(Ok(("resources/list".into(), json!({})))),
        "__mcp_read_resource" => {
            let uri = payload
                .get("uri")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| AppError::BadRequest("read_resource requires payload.uri".into()));
            Some(uri.map(|uri| ("resources/read".into(), json!({"uri": uri}))))
        }
        "__mcp_list_prompts" => Some(Ok(("prompts/list".into(), json!({})))),
        "__mcp_get_prompt" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| AppError::BadRequest("get_prompt requires payload.name".into()));
            Some(name.map(|name| {
                let arguments = payload
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                (
                    "prompts/get".into(),
                    json!({"name": name, "arguments": arguments}),
                )
            }))
        }
        _ => None,
    }
}

fn sanitize_mcp_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn requires_approval(tool_name: &str, description: Option<&str>) -> bool {
    let haystack = format!("{} {}", tool_name, description.unwrap_or_default()).to_lowercase();
    let safe_read = [
        "snapshot", "list", "read", "get", "search", "query", "find", "inspect", "open", "fetch",
        "status", "metadata", "schema",
    ];
    if safe_read.iter().any(|keyword| haystack.contains(keyword)) {
        return false;
    }
    let high_risk = [
        "shell",
        "terminal",
        "exec",
        "execute",
        "command",
        "write",
        "patch",
        "delete",
        "remove",
        "rm",
        "move",
        "rename",
        "chmod",
        "chown",
        "kill",
        "install",
        "uninstall",
        "deploy",
        "payment",
        "email",
        "send",
        "submit",
    ];
    high_risk.iter().any(|keyword| haystack.contains(keyword))
}

fn get_server(store: &AppStore, server_id: &str) -> AppResult<McpServer> {
    mcp_servers(store)?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| AppError::NotFound(format!("mcp server {server_id}")))
}

fn mcp_servers(store: &AppStore) -> AppResult<Vec<McpServer>> {
    Ok(store
        .static_list("mcpServers")?
        .into_iter()
        .filter_map(|value| serde_json::from_value::<McpServer>(value).ok())
        .collect())
}

fn merge_tool_definitions(
    store: &AppStore,
    server_id: &str,
    mut definitions: Vec<ToolDefinition>,
) -> AppResult<()> {
    let mut all = store.tool_definitions()?;
    all.retain(|definition| definition.server_id != server_id);
    all.append(&mut definitions);
    store.set_tool_definitions(all)?;
    Ok(())
}

async fn one_shot_json(server: &McpServer, payload: Value) -> AppResult<Value> {
    let mut child = spawn_mcp_server(server).await?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(payload.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Err(AppError::BadRequest(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    parse_json_stdout(&output.stdout)
}

async fn mcp_json_rpc_tools_list(server: &McpServer) -> AppResult<Value> {
    let mut child = spawn_mcp_server(server).await?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();

    write_rpc(
        &mut stdin,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "SynthChat", "version": "1.0.0"}
        }),
    )
    .await?;
    read_response(&mut lines, 1).await?;
    stdin
        .write_all(
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
                .to_string()
                .as_bytes(),
        )
        .await?;
    stdin.write_all(b"\n").await?;
    write_rpc(&mut stdin, 2, "tools/list", json!({})).await?;
    let response = read_response(&mut lines, 2).await?;
    let _ = child.kill().await;
    Ok(response)
}

async fn mcp_json_rpc_call(
    server: &McpServer,
    tool_name: &str,
    payload: Value,
) -> AppResult<Value> {
    let mut child = spawn_mcp_server(server).await?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();

    write_rpc(
        &mut stdin,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "SynthChat", "version": "1.0.0"}
        }),
    )
    .await?;
    read_response(&mut lines, 1).await?;
    stdin
        .write_all(
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
                .to_string()
                .as_bytes(),
        )
        .await?;
    stdin.write_all(b"\n").await?;
    write_rpc(
        &mut stdin,
        2,
        "tools/call",
        json!({"name": tool_name, "arguments": payload}),
    )
    .await?;
    let response = read_response(&mut lines, 2).await?;
    let _ = child.kill().await;
    Ok(response)
}

async fn mcp_json_rpc_method(server: &McpServer, method: &str, params: Value) -> AppResult<Value> {
    let mut child = spawn_mcp_server(server).await?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::BadRequest("missing mcp stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();

    write_rpc(
        &mut stdin,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "SynthChat", "version": "1.0.0"}
        }),
    )
    .await?;
    read_response(&mut lines, 1).await?;
    stdin
        .write_all(
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
                .to_string()
                .as_bytes(),
        )
        .await?;
    stdin.write_all(b"\n").await?;
    write_rpc(&mut stdin, 2, method, params).await?;
    let response = read_response(&mut lines, 2).await?;
    let _ = child.kill().await;
    Ok(response)
}

async fn mcp_http_json_rpc_method(
    server: &McpServer,
    method: &str,
    params: Value,
) -> AppResult<Value> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest(format!("mcp server {} missing url", server.id)))?;
    let client = reqwest::Client::new();
    let mut request = client
        .post(url)
        .header(
            "Accept",
            "application/json, application/x-ndjson, text/event-stream",
        )
        .json(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
        }));
    if let Some(headers) = &server.headers {
        for (name, value) in headers {
            request = request.header(name, value);
        }
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("mcp http request failed: {error}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("mcp http response read failed: {error}")))?;
    let value = parse_mcp_http_response_value(&server.id, status, &text)?;
    if let Some(error) = value.get("error") {
        return Err(AppError::BadRequest(format!(
            "mcp server {} returned JSON-RPC error: {}",
            server.id, error
        )));
    }
    Ok(value.get("result").cloned().unwrap_or(value))
}

fn parse_mcp_http_response_value(
    server_id: &str,
    status: reqwest::StatusCode,
    text: &str,
) -> AppResult<Value> {
    let values = parse_mcp_http_json_values(text)?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "mcp server {} returned HTTP {}: {}",
            server_id,
            status,
            values
                .last()
                .cloned()
                .unwrap_or_else(|| Value::String(text.trim().to_string()))
        )));
    }
    values
        .into_iter()
        .find(|value| value.get("id").and_then(Value::as_u64) == Some(1))
        .or_else(|| {
            parse_mcp_http_json_values(text)
                .ok()
                .and_then(|mut values| values.pop())
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "mcp http response parse failed: no JSON-RPC message in response body"
            ))
        })
}

fn parse_mcp_http_json_values(text: &str) -> AppResult<Vec<Value>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(vec![value]);
    }
    let mut values = Vec::new();
    let mut sse_data = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            parse_sse_data_lines(&mut sse_data, &mut values)?;
            continue;
        }
        if let Some(data) = trimmed.strip_prefix("data:") {
            sse_data.push(data.trim().to_string());
            continue;
        }
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                values.push(value);
            }
        }
    }
    parse_sse_data_lines(&mut sse_data, &mut values)?;
    if values.is_empty() {
        return Err(AppError::BadRequest(
            "mcp http response parse failed: expected JSON, NDJSON, or SSE data JSON".into(),
        ));
    }
    Ok(values)
}

fn parse_sse_data_lines(lines: &mut Vec<String>, values: &mut Vec<Value>) -> AppResult<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let data = lines.join("\n");
    lines.clear();
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let value = serde_json::from_str::<Value>(data).map_err(|error| {
        AppError::BadRequest(format!(
            "mcp http SSE data parse failed: {error}; data: {data}"
        ))
    })?;
    values.push(value);
    Ok(())
}

fn command(server: &McpServer) -> Command {
    let mut cmd = Command::new(&server.command);
    cmd.args(&server.args);
    if let Some(env) = &server.env {
        cmd.envs(env);
    }
    cmd.kill_on_drop(true);
    cmd
}

async fn spawn_mcp_server(server: &McpServer) -> AppResult<Child> {
    if let Some(block_reason) = check_mcp_package_for_malware(&server.command, &server.args).await {
        return Err(AppError::BadRequest(block_reason));
    }
    command(server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::BadRequest(format!("failed to start {}: {e}", server.command)))
}

async fn check_mcp_package_for_malware(command: &str, args: &[String]) -> Option<String> {
    let ecosystem = infer_osv_ecosystem(command)?;
    let (package, version) = parse_osv_package_from_args(args, ecosystem)?;
    match query_osv_malware(&package, ecosystem, version.as_deref()).await {
        Ok(malware) if !malware.is_empty() => {
            let ids = malware
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            let summaries = malware
                .iter()
                .filter_map(|item| {
                    item.get("summary")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                })
                .take(3)
                .map(|text| text.chars().take(100).collect::<String>())
                .collect::<Vec<_>>()
                .join("; ");
            Some(format!(
                "BLOCKED: MCP package '{package}' ({ecosystem}) has known malware advisories: {ids}. Details: {summaries}"
            ))
        }
        _ => None,
    }
}

pub(crate) fn infer_osv_ecosystem(command: &str) -> Option<&'static str> {
    let base = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    match base.as_str() {
        "npx" | "npx.cmd" | "npx.exe" => Some("npm"),
        "uvx" | "uvx.cmd" | "uvx.exe" | "pipx" | "pipx.exe" => Some("PyPI"),
        _ => None,
    }
}

pub(crate) fn parse_osv_package_from_args(
    args: &[String],
    ecosystem: &str,
) -> Option<(String, Option<String>)> {
    let token = args
        .iter()
        .map(|arg| arg.trim())
        .find(|arg| !arg.is_empty() && !arg.starts_with('-'))?;
    match ecosystem {
        "npm" => parse_npm_package_token(token),
        "PyPI" => parse_pypi_package_token(token),
        _ => Some((token.to_string(), None)),
    }
}

pub(crate) fn parse_npm_package_token(token: &str) -> Option<(String, Option<String>)> {
    if token.starts_with('@') {
        let slash = token.find('/')?;
        let rest = &token[slash + 1..];
        if let Some(at) = rest.rfind('@') {
            let name_end = slash + 1 + at;
            let version = &rest[at + 1..];
            let version = (!version.is_empty() && version != "latest").then(|| version.to_string());
            return Some((token[..name_end].to_string(), version));
        }
        return Some((token.to_string(), None));
    }
    if let Some((name, version)) = token.rsplit_once('@') {
        if !name.is_empty() {
            let version = (!version.is_empty() && version != "latest").then(|| version.to_string());
            return Some((name.to_string(), version));
        }
    }
    Some((token.to_string(), None))
}

pub(crate) fn parse_pypi_package_token(token: &str) -> Option<(String, Option<String>)> {
    let (name_part, version) = token
        .split_once("==")
        .map(|(name, version)| (name, Some(version.to_string())))
        .unwrap_or((token, None));
    let name = name_part
        .split_once('[')
        .map(|(name, _)| name)
        .unwrap_or(name_part)
        .trim();
    (!name.is_empty()).then(|| (name.to_string(), version))
}

pub(crate) async fn query_osv_malware(
    package: &str,
    ecosystem: &str,
    version: Option<&str>,
) -> AppResult<Vec<Value>> {
    let endpoint =
        std::env::var("OSV_ENDPOINT").unwrap_or_else(|_| "https://api.osv.dev/v1/query".into());
    let mut body = json!({"package": {"name": package, "ecosystem": ecosystem}});
    if let Some(version) = version.filter(|value| !value.trim().is_empty()) {
        body["version"] = json!(version);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| AppError::BadRequest(format!("OSV client error: {error}")))?;
    let response = client
        .post(endpoint)
        .header("User-Agent", "synthchat-osv-check/1.0")
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("OSV request error: {error}")))?;
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| AppError::BadRequest(format!("OSV response error: {error}")))?;
    Ok(value
        .get("vulns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .map(|id| id.starts_with("MAL-"))
                .unwrap_or(false)
        })
        .collect())
}

async fn write_rpc(
    stdin: &mut tokio::process::ChildStdin,
    id: u64,
    method: &str,
    params: Value,
) -> AppResult<()> {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> AppResult<Value> {
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|e| AppError::BadRequest(format!("invalid mcp json: {e}; line: {trimmed}")))?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = value.get("error") {
                return Err(AppError::BadRequest(format!("mcp error: {error}")));
            }
            return Ok(value.get("result").cloned().unwrap_or(value));
        }
    }
    Err(AppError::BadRequest(
        "mcp process exited before response".into(),
    ))
}

fn parse_json_stdout(stdout: &[u8]) -> AppResult<Value> {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(trimmed) {
            return Ok(value);
        }
    }
    Err(AppError::BadRequest(format!(
        "stdout did not contain json: {}",
        text.chars().take(500).collect::<String>()
    )))
}

fn parse_tools(raw: &Value) -> Vec<McpToolInfo> {
    let tools = raw
        .get("tools")
        .or_else(|| raw.pointer("/result/tools"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    tools
        .into_iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_string();
            Some(McpToolInfo {
                name,
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                input_schema: tool
                    .get("inputSchema")
                    .or_else(|| tool.get("input_schema"))
                    .cloned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mcp_server(id: &str) -> McpServer {
        McpServer {
            id: id.into(),
            name: id.into(),
            transport: None,
            command: String::new(),
            args: vec![],
            env: None,
            url: None,
            headers: None,
            protocol: "jsonRpc".into(),
            enabled: true,
            timeout_seconds: 10,
            supports_parallel_tool_calls: false,
        }
    }

    #[test]
    fn mcp_utility_definitions_use_hermes_server_scoped_names() {
        let definitions = mcp_utility_tool_definitions(&test_mcp_server("ai.exa/exa"));
        let names = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "mcp_ai_exa_exa_list_resources",
                "mcp_ai_exa_exa_read_resource",
                "mcp_ai_exa_exa_list_prompts",
                "mcp_ai_exa_exa_get_prompt"
            ]
        );
        assert!(definitions
            .iter()
            .all(|definition| definition.source == "mcp_utility"));
        assert!(definitions
            .iter()
            .all(|definition| !definition.requires_approval));
    }

    #[test]
    fn mcp_utility_request_maps_resource_and_prompt_methods() {
        assert_eq!(
            mcp_utility_request("__mcp_list_resources", json!({}))
                .unwrap()
                .unwrap(),
            ("resources/list".into(), json!({}))
        );
        assert_eq!(
            mcp_utility_request("__mcp_read_resource", json!({"uri": " file://doc.md "}))
                .unwrap()
                .unwrap(),
            ("resources/read".into(), json!({"uri": "file://doc.md"}))
        );
        assert_eq!(
            mcp_utility_request("__mcp_list_prompts", json!({}))
                .unwrap()
                .unwrap(),
            ("prompts/list".into(), json!({}))
        );
        assert_eq!(
            mcp_utility_request(
                "__mcp_get_prompt",
                json!({"name": " summarize ", "arguments": {"topic": "mcp"}})
            )
            .unwrap()
            .unwrap(),
            (
                "prompts/get".into(),
                json!({"name": "summarize", "arguments": {"topic": "mcp"}})
            )
        );
    }

    #[test]
    fn mcp_utility_request_rejects_missing_required_fields() {
        assert!(mcp_utility_request("__mcp_read_resource", json!({}))
            .unwrap()
            .is_err());
        assert!(
            mcp_utility_request("__mcp_get_prompt", json!({"arguments": {}}))
                .unwrap()
                .is_err()
        );
        assert!(mcp_utility_request("search-docs", json!({})).is_none());
    }

    #[test]
    fn mcp_http_parser_accepts_sse_data_json() {
        let body = "event: message\n\
data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"search\"}]}}\n\n";
        let value = parse_mcp_http_response_value("srv", reqwest::StatusCode::OK, body).unwrap();
        assert_eq!(value["result"]["tools"][0]["name"], "search");
    }

    #[test]
    fn mcp_http_parser_accepts_ndjson_and_selects_response_id() {
        let body = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\
{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n";
        let value = parse_mcp_http_response_value("srv", reqwest::StatusCode::OK, body).unwrap();
        assert_eq!(value["result"]["content"][0]["text"], "ok");
    }

    #[test]
    fn osv_ecosystem_detection_matches_mcp_package_runners() {
        assert_eq!(infer_osv_ecosystem("npx"), Some("npm"));
        assert_eq!(infer_osv_ecosystem("C:\\tools\\npx.cmd"), Some("npm"));
        assert_eq!(infer_osv_ecosystem("uvx"), Some("PyPI"));
        assert_eq!(infer_osv_ecosystem("pipx.exe"), Some("PyPI"));
        assert_eq!(infer_osv_ecosystem("node"), None);
    }

    #[test]
    fn osv_package_parser_handles_npm_and_pypi_tokens() {
        assert_eq!(
            parse_osv_package_from_args(&["-y".into(), "@scope/server@1.2.3".into()], "npm"),
            Some(("@scope/server".into(), Some("1.2.3".into())))
        );
        assert_eq!(
            parse_osv_package_from_args(&["server@latest".into()], "npm"),
            Some(("server".into(), None))
        );
        assert_eq!(
            parse_osv_package_from_args(&["pkg[extra]==0.1.0".into()], "PyPI"),
            Some(("pkg".into(), Some("0.1.0".into())))
        );
    }
}
