use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, Command},
};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, AgentDefinition},
    process_utils::CommandWindowExt,
};

use super::{
    truncate_output,
    workspace::{resolve_workspace_path, workspace_root},
};
pub(super) fn edit_diagnostics_for_paths(
    agent: &AgentDefinition,
    root: &Path,
    paths: &[PathBuf],
) -> AppResult<Value> {
    edit_diagnostics_for_paths_with_baselines(agent, root, paths, |_| None)
}

pub(super) fn edit_diagnostics_for_paths_with_baselines<'a>(
    agent: &AgentDefinition,
    root: &Path,
    paths: &[PathBuf],
    pre_content_for_path: impl Fn(&Path) -> Option<&'a str>,
) -> AppResult<Value> {
    let mut files = Vec::new();
    let mut modes = BTreeSet::new();
    for path in paths {
        if !path.exists() || path.is_dir() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut diagnostics = Vec::new();
        let pre_content = pre_content_for_path(path);
        let post_content = pre_content.and_then(|_| fs::read_to_string(path).ok());
        if ext == "json" {
            let pre = pre_content.map(json_syntax_diagnostic_for_content);
            let shifted_line = shifted_baseline_diagnostic_line(
                pre_content,
                post_content.as_deref(),
                pre.as_ref(),
            );
            diagnostics.push(refine_syntax_diagnostic_delta(
                json_syntax_diagnostic(path),
                pre,
                shifted_line,
            ));
        }
        if ext == "py" {
            let pre = pre_content.map(|content| {
                python_backed_content_syntax_diagnostic(
                    path,
                    content,
                    "py",
                    "python ast.parse",
                    "Python syntax ok",
                    PYTHON_AST_PARSE_SCRIPT,
                )
            });
            let shifted_line = shifted_baseline_diagnostic_line(
                pre_content,
                post_content.as_deref(),
                pre.as_ref(),
            );
            diagnostics.push(refine_syntax_diagnostic_delta(
                python_syntax_diagnostic(path),
                pre,
                shifted_line,
            ));
        }
        if ext == "toml" {
            let pre = pre_content.map(|content| {
                python_backed_content_syntax_diagnostic(
                    path,
                    content,
                    "toml",
                    "python tomllib",
                    "TOML syntax ok",
                    TOML_PARSE_SCRIPT,
                )
            });
            let shifted_line = shifted_baseline_diagnostic_line(
                pre_content,
                post_content.as_deref(),
                pre.as_ref(),
            );
            diagnostics.push(refine_syntax_diagnostic_delta(
                toml_syntax_diagnostic(path),
                pre,
                shifted_line,
            ));
        }
        if matches!(ext.as_str(), "yaml" | "yml") {
            let pre = pre_content.map(|content| {
                python_backed_content_syntax_diagnostic(
                    path,
                    content,
                    ext.as_str(),
                    "python PyYAML",
                    "YAML syntax ok",
                    YAML_PARSE_SCRIPT,
                )
            });
            let shifted_line = shifted_baseline_diagnostic_line(
                pre_content,
                post_content.as_deref(),
                pre.as_ref(),
            );
            diagnostics.push(refine_syntax_diagnostic_delta(
                yaml_syntax_diagnostic(path),
                pre,
                shifted_line,
            ));
        }
        if matches!(ext.as_str(), "js" | "mjs" | "cjs") {
            let pre = pre_content
                .map(|content| node_content_syntax_diagnostic(path, content, ext.as_str()));
            let shifted_line = shifted_baseline_diagnostic_line(
                pre_content,
                post_content.as_deref(),
                pre.as_ref(),
            );
            diagnostics.push(refine_syntax_diagnostic_delta(
                node_syntax_diagnostic(path),
                pre,
                shifted_line,
            ));
        }
        if edit_extension_recommends_workspace_diagnostics(&ext) {
            if let Some(mode) = workspace_diagnostics_mode_for_extension(root, &ext) {
                modes.insert(mode);
            }
        }
        if !diagnostics.is_empty() {
            files.push(json!({
                "path": relative,
                "diagnostics": diagnostics
            }));
        }
    }

    let recommended_mode = if modes.contains("rust") && modes.contains("typescript") {
        Some("all")
    } else {
        modes.iter().next().copied()
    };
    let workspace = recommended_mode.map(|mode| {
        let payload = json!({
            "mode": mode,
            "workspaceDir": ".",
            "timeoutSeconds": 30,
            "maxCommands": 2
        });
        let mut value = json!({
            "recommended": true,
            "tool": "workspace_diagnostics",
            "payload": payload,
            "reason": "Run after code edits to catch build, type, or lint errors not covered by in-process syntax checks."
        });
        if let Some(result) = run_workspace_diagnostics_after_edit(agent, &payload) {
            value["result"] = result;
        }
        value
    });

    Ok(json!({
        "files": files,
        "workspaceDiagnostics": workspace.unwrap_or_else(|| json!({"recommended": false}))
    }))
}

fn json_syntax_diagnostic(path: &Path) -> Value {
    match fs::read_to_string(path) {
        Ok(content) => json_syntax_diagnostic_for_content(&content),
        Err(error) => json!({
            "kind": "syntax",
            "ok": false,
            "message": format!("failed to read file for diagnostics: {error}")
        }),
    }
}

fn json_syntax_diagnostic_for_content(content: &str) -> Value {
    match serde_json::from_str::<Value>(content) {
        Ok(_) => json!({
            "kind": "syntax",
            "tool": "serde_json",
            "ok": true,
            "message": "JSON syntax ok"
        }),
        Err(error) => json!({
            "kind": "syntax",
            "tool": "serde_json",
            "ok": false,
            "message": error.to_string(),
            "line": error.line(),
            "column": error.column()
        }),
    }
}

const PYTHON_AST_PARSE_SCRIPT: &str = r#"
import ast
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    source = path.read_text(encoding="utf-8-sig")
    ast.parse(source, filename=str(path))
except SyntaxError as error:
    print(f"{type(error).__name__}: {error.msg} (line {error.lineno}, column {error.offset})")
    sys.exit(1)
except Exception as error:
    print(f"{type(error).__name__}: {error}")
    sys.exit(2)
"#;

const TOML_PARSE_SCRIPT: &str = r#"
import pathlib
import sys

try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        print("__SKIP__: TOML parser not available")
        sys.exit(3)

path = pathlib.Path(sys.argv[1])
try:
    tomllib.loads(path.read_text(encoding="utf-8-sig"))
except Exception as error:
    print(f"{type(error).__name__}: {error}")
    sys.exit(1)
"#;

const YAML_PARSE_SCRIPT: &str = r#"
import pathlib
import sys

try:
    import yaml
except ImportError:
    print("__SKIP__: PyYAML not available")
    sys.exit(3)

path = pathlib.Path(sys.argv[1])
try:
    yaml.safe_load(path.read_text(encoding="utf-8-sig"))
except yaml.YAMLError as error:
    print(f"YAMLError: {error}")
    sys.exit(1)
except Exception as error:
    print(f"{type(error).__name__}: {error}")
    sys.exit(2)
"#;

fn python_syntax_diagnostic(path: &Path) -> Value {
    python_backed_syntax_diagnostic(
        path,
        "python ast.parse",
        "Python syntax ok",
        PYTHON_AST_PARSE_SCRIPT,
    )
}

fn toml_syntax_diagnostic(path: &Path) -> Value {
    python_backed_syntax_diagnostic(path, "python tomllib", "TOML syntax ok", TOML_PARSE_SCRIPT)
}

fn yaml_syntax_diagnostic(path: &Path) -> Value {
    python_backed_syntax_diagnostic(path, "python PyYAML", "YAML syntax ok", YAML_PARSE_SCRIPT)
}

fn node_syntax_diagnostic(path: &Path) -> Value {
    match run_path_syntax_command("node", &["--check"], path, Duration::from_secs(8)) {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout).to_string()
                + &String::from_utf8_lossy(&output.stderr);
            let message = strip_ansi_codes(text.trim()).trim().to_string();
            json!({
                "kind": "syntax",
                "tool": "node --check",
                "ok": output.status.success(),
                "message": if output.status.success() {
                    "JavaScript syntax ok".to_string()
                } else if message.is_empty() {
                    "node --check failed".to_string()
                } else {
                    message
                }
            })
        }
        Err(error) => json!({
            "kind": "syntax",
            "tool": "node --check",
            "ok": true,
            "skipped": true,
            "message": format!("node --check skipped: {error}")
        }),
    }
}

fn node_content_syntax_diagnostic(original_path: &Path, content: &str, ext: &str) -> Value {
    let path =
        std::env::temp_dir().join(format!("synthchat-diagnostic-{}.{}", new_id("diag"), ext));
    if let Err(error) = fs::write(&path, content) {
        return json!({
            "kind": "syntax",
            "tool": "node --check",
            "ok": true,
            "skipped": true,
            "message": format!("baseline syntax check skipped for {}: {error}", original_path.display())
        });
    }
    let diagnostic = node_syntax_diagnostic(&path);
    let _ = fs::remove_file(&path);
    diagnostic
}

fn python_backed_content_syntax_diagnostic(
    original_path: &Path,
    content: &str,
    ext: &str,
    tool: &str,
    ok_message: &str,
    script: &str,
) -> Value {
    let suffix = if ext.is_empty() {
        "txt".to_string()
    } else {
        ext.to_string()
    };
    let path = std::env::temp_dir().join(format!(
        "synthchat-diagnostic-{}.{}",
        new_id("diag"),
        suffix
    ));
    if let Err(error) = fs::write(&path, content) {
        return json!({
            "kind": "syntax",
            "tool": tool,
            "ok": true,
            "skipped": true,
            "message": format!("baseline syntax check skipped for {}: {error}", original_path.display())
        });
    }
    let diagnostic = python_backed_syntax_diagnostic(&path, tool, ok_message, script);
    let _ = fs::remove_file(&path);
    diagnostic
}

fn shifted_baseline_diagnostic_line(
    pre_content: Option<&str>,
    post_content: Option<&str>,
    pre: Option<&Value>,
) -> Option<usize> {
    let pre_line = pre?
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|line| usize::try_from(line).ok())?;
    if pre_line == 0 {
        return None;
    }
    let shift = build_line_shift(pre_content?, post_content?);
    shift(pre_line - 1).map(|line| line + 1)
}

pub(super) fn build_line_shift(
    pre_text: &str,
    post_text: &str,
) -> Box<dyn Fn(usize) -> Option<usize>> {
    let pre_lines = pre_text.lines().map(str::to_string).collect::<Vec<_>>();
    let post_lines = post_text.lines().map(str::to_string).collect::<Vec<_>>();
    if pre_lines == post_lines {
        return Box::new(move |line| Some(line));
    }
    let matches = lcs_line_matches(&pre_lines, &post_lines);
    Box::new(move |line| {
        if line >= pre_lines.len() {
            return post_lines.len().checked_sub(1);
        }
        if let Some((_, post_index)) = matches.iter().find(|(pre_index, _)| *pre_index == line) {
            return Some(*post_index);
        }
        None
    })
}

fn lcs_line_matches(pre_lines: &[String], post_lines: &[String]) -> Vec<(usize, usize)> {
    let rows = pre_lines.len();
    let cols = post_lines.len();
    let mut table = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            table[i][j] = if pre_lines[i] == post_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut matches = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < rows && j < cols {
        if pre_lines[i] == post_lines[j] {
            matches.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    matches
}

fn refine_syntax_diagnostic_delta(
    mut post: Value,
    pre: Option<Value>,
    shifted_baseline_line: Option<usize>,
) -> Value {
    let Some(pre) = pre else {
        return post;
    };
    if post.get("ok").and_then(Value::as_bool).unwrap_or(true) {
        return post;
    }
    if post
        .get("skipped")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return post;
    }
    let pre_ok = pre.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let post_message = post
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let pre_message = pre
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let post_line = post
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|line| usize::try_from(line).ok());
    let baseline_equivalent = pre_message == post_message
        || (shifted_baseline_line == post_line
            && normalize_syntax_message_for_delta(&pre_message)
                == normalize_syntax_message_for_delta(&post_message));
    if let Some(object) = post.as_object_mut() {
        object.insert("baselineChecked".into(), json!(true));
        object.insert("baselineOk".into(), json!(pre_ok));
        if !pre_ok {
            object.insert("baselineMessage".into(), json!(pre_message));
            if let Some(line) = shifted_baseline_line {
                object.insert("baselineShiftedLine".into(), json!(line));
            }
            object.insert(
                "introducedByEdit".into(),
                json!(pre_message.is_empty() || !baseline_equivalent),
            );
            if baseline_equivalent {
                object.insert(
                    "deltaMessage".into(),
                    json!(
                        "Pre-existing syntax error; this edit did not introduce a new first reported syntax error."
                    ),
                );
            } else {
                object.insert(
                    "deltaMessage".into(),
                    json!("Syntax error differs from baseline and may have been introduced by this edit."),
                );
            }
        } else {
            object.insert("introducedByEdit".into(), json!(true));
            object.insert(
                "deltaMessage".into(),
                json!("New syntax error introduced by this edit."),
            );
        }
    }
    post
}

fn normalize_syntax_message_for_delta(message: &str) -> String {
    let mut output = Vec::new();
    let mut skip_number_after = "";
    for token in message.split_whitespace() {
        if skip_number_after == "line" && token.chars().all(|ch| ch.is_ascii_digit()) {
            output.push("<line>");
            skip_number_after = "";
            continue;
        }
        if skip_number_after == "column" && token.chars().all(|ch| ch.is_ascii_digit()) {
            output.push("<column>");
            skip_number_after = "";
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if lower == "line" || lower == "column" {
            skip_number_after = if lower == "line" { "line" } else { "column" };
        } else {
            skip_number_after = "";
        }
        output.push(token);
    }
    output.join(" ")
}

fn python_backed_syntax_diagnostic(
    path: &Path,
    tool: &str,
    ok_message: &str,
    script: &str,
) -> Value {
    let candidates: Vec<(&str, Vec<&str>)> = if cfg!(windows) {
        vec![
            ("python", vec!["-c", script]),
            ("py", vec!["-3", "-c", script]),
        ]
    } else {
        vec![
            ("python3", vec!["-c", script]),
            ("python", vec!["-c", script]),
        ]
    };
    let mut unavailable = Vec::new();
    for (program, args) in candidates {
        match run_path_syntax_command(program, &args, path, Duration::from_secs(8)) {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout).to_string()
                    + &String::from_utf8_lossy(&output.stderr);
                let message = strip_ansi_codes(text.trim()).trim().to_string();
                if message.starts_with("__SKIP__:") {
                    return json!({
                        "kind": "syntax",
                        "tool": tool,
                        "ok": true,
                        "skipped": true,
                        "message": message.trim_start_matches("__SKIP__:").trim()
                    });
                }
                return json!({
                    "kind": "syntax",
                    "tool": format!("{program} {tool}"),
                    "ok": output.status.success(),
                    "message": if output.status.success() {
                        ok_message.to_string()
                    } else if message.is_empty() {
                        format!("{tool} check failed")
                    } else {
                        message
                    }
                });
            }
            Err(error) => unavailable.push(format!("{program}: {error}")),
        }
    }
    json!({
        "kind": "syntax",
        "tool": tool,
        "ok": true,
        "skipped": true,
        "message": format!("{tool} check skipped: {}", unavailable.join("; "))
    })
}

fn run_path_syntax_command(
    program: &str,
    args: &[&str],
    path: &Path,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = StdCommand::new(program)
        .hide_window()
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|error| error.to_string());
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn run_workspace_diagnostics_after_edit(agent: &AgentDefinition, payload: &Value) -> Option<Value> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let result =
        tokio::task::block_in_place(|| handle.block_on(workspace_diagnostics_tool(agent, payload)));
    Some(match result {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"raw": text})),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string()
        }),
    })
}

fn edit_extension_recommends_workspace_diagnostics(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "toml"
            | "go"
            | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "vue"
            | "svelte"
    )
}

pub(super) fn workspace_diagnostics_mode_for_extension(
    root: &Path,
    ext: &str,
) -> Option<&'static str> {
    match ext {
        "rs" => root.join("Cargo.toml").exists().then_some("rust"),
        "toml" => root.join("Cargo.toml").exists().then_some("rust"),
        "go" => go_workspace_detected(root).then_some("go"),
        "py" => python_workspace_detected(root).then_some("python"),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => {
            root.join("tsconfig.json").exists().then_some("typescript")
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiagnosticCommand {
    pub(super) family: &'static str,
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) display: String,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedDiagnostic {
    pub(super) file: String,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) severity: String,
    pub(super) code: Option<String>,
    pub(super) message: String,
    pub(super) source: String,
}

const LSP_MAX_PER_FILE: usize = 20;
const LSP_MAX_TOTAL_CHARS: usize = 4000;
const LSP_INITIALIZE_TIMEOUT_SECONDS: u64 = 45;
const LSP_DIAGNOSTICS_WAIT_SECONDS: u64 = 5;
const LSP_IDLE_TIMEOUT_SECONDS: u64 = 600;
const LSP_MAX_HEADER_BYTES: usize = 8 * 1024;
const LSP_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub(super) async fn workspace_diagnostics_tool(
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let root = diagnostics_workspace(agent, payload)?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("run")
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    if matches!(
        action.as_str(),
        "status" | "list" | "lsp_status" | "lsp_list"
    ) {
        let installed_only = payload
            .get("installedOnly")
            .or_else(|| payload.get("installed_only"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return lsp_status_report(&root, installed_only);
    }
    if matches!(
        action.as_str(),
        "which"
            | "lsp_which"
            | "start"
            | "lsp_start"
            | "stop"
            | "lsp_stop"
            | "restart"
            | "lsp_restart"
            | "clients"
            | "lsp_clients"
            | "install"
            | "lsp_install"
            | "install_all"
            | "lsp_install_all"
            | "diagnostics"
            | "lsp_diagnostics"
            | "did_open"
            | "lsp_did_open"
            | "baseline"
            | "snapshot_baseline"
            | "lsp_snapshot_baseline"
            | "clear_baseline"
            | "lsp_clear_baseline"
    ) {
        return lsp_lifecycle_action(&root, payload, &action).await;
    }
    let mode = diagnostics_mode(payload);
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(90)
        .clamp(5, 240);
    let max_commands = payload
        .get("maxCommands")
        .or_else(|| payload.get("max_commands"))
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 5) as usize;
    let commands = diagnostic_commands_for_workspace(&root, &mode)
        .into_iter()
        .take(max_commands)
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return Ok(serde_json::to_string_pretty(&json!({
            "workspace": root.to_string_lossy(),
            "mode": mode,
            "ok": true,
            "commands": [],
            "message": "No supported diagnostics detected. Expected Cargo.toml, tsconfig.json, go.mod, pyproject.toml, setup.py, requirements.txt, or pyrightconfig.json."
        }))?);
    }

    let mut all_ok = true;
    let mut rendered = Vec::new();
    let mut raw_commands = Vec::new();
    for command in commands {
        let started = Instant::now();
        let result = run_diagnostic_command(&root, &command, timeout_seconds).await;
        let elapsed_ms = started.elapsed().as_millis();
        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let ok = output.status.success();
                let exit_code = output.status.code();
                let diagnostics = parse_command_diagnostics(command.family, &stdout, &stderr);
                let diagnostics_text = format_diagnostics_block(&diagnostics);
                let lsp_diagnostics = diagnostics_to_lsp_json(&diagnostics);
                let lsp_diagnostics_text = format_lsp_diagnostics_report(&lsp_diagnostics);
                all_ok &= ok;
                rendered.push(format!(
                    "$ {}\nstatus={} elapsedMs={}\nstdout:\n{}\nstderr:\n{}",
                    command.display,
                    exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "terminated".into()),
                    elapsed_ms,
                    truncate_output(&stdout, 5000),
                    truncate_output(&stderr, 5000)
                ));
                raw_commands.push(json!({
                    "family": command.family,
                    "command": command.display,
                    "ok": ok,
                    "timedOut": false,
                    "exitCode": exit_code,
                    "elapsedMs": elapsed_ms,
                    "diagnostics": diagnostics_to_json(&diagnostics),
                    "diagnosticsText": diagnostics_text,
                    "lspDiagnostics": lsp_diagnostics,
                    "lspDiagnosticsText": lsp_diagnostics_text,
                    "stdout": truncate_output(&stdout, 12000),
                    "stderr": truncate_output(&stderr, 12000)
                }));
            }
            Err(error) => {
                all_ok = false;
                rendered.push(format!(
                    "$ {}\nstatus=error elapsedMs={}\nerror={}",
                    command.display, elapsed_ms, error
                ));
                raw_commands.push(json!({
                    "family": command.family,
                    "command": command.display,
                    "ok": false,
                    "timedOut": error.contains("timed out"),
                    "elapsedMs": elapsed_ms,
                    "error": error
                }));
            }
        }
    }

    let diagnostics_text = raw_commands
        .iter()
        .filter_map(|command| command.get("diagnosticsText").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let lsp_diagnostics_text = raw_commands
        .iter()
        .filter_map(|command| command.get("lspDiagnosticsText").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(serde_json::to_string_pretty(&json!({
        "workspace": root.to_string_lossy(),
        "mode": mode,
        "ok": all_ok,
        "timeoutSeconds": timeout_seconds,
        "commands": raw_commands,
        "diagnosticsText": diagnostics_text,
        "lspDiagnosticsText": lsp_diagnostics_text,
        "text": rendered.join("\n\n---\n\n")
    }))?)
}

struct LspServerInfo {
    server_id: &'static str,
    package: &'static str,
    extensions: &'static [&'static str],
    binaries: &'static [&'static str],
    spawn_args: &'static [&'static str],
    description: &'static str,
    install_hint: &'static str,
}

struct LspInstallRecipe {
    command: &'static str,
    args: &'static [&'static str],
}

impl LspInstallRecipe {
    fn display(&self) -> String {
        std::iter::once(self.command)
            .chain(self.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

const LSP_SERVERS: &[LspServerInfo] = &[
    LspServerInfo {
        server_id: "pyright",
        package: "pyright",
        extensions: &["py", "pyi"],
        binaries: &["pyright-langserver", "pyright"],
        spawn_args: &["--stdio"],
        description: "Python semantic diagnostics and type checking.",
        install_hint: "npm install -g pyright or add pyright to the workspace environment",
    },
    LspServerInfo {
        server_id: "typescript",
        package: "typescript-language-server",
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        binaries: &["typescript-language-server"],
        spawn_args: &["--stdio"],
        description: "TypeScript and JavaScript language server diagnostics.",
        install_hint: "npm install -g typescript typescript-language-server",
    },
    LspServerInfo {
        server_id: "gopls",
        package: "gopls",
        extensions: &["go"],
        binaries: &["gopls"],
        spawn_args: &[],
        description: "Go diagnostics powered by gopls.",
        install_hint: "go install golang.org/x/tools/gopls@latest",
    },
    LspServerInfo {
        server_id: "rust-analyzer",
        package: "rust-analyzer",
        extensions: &["rs"],
        binaries: &["rust-analyzer"],
        spawn_args: &[],
        description: "Rust diagnostics powered by rust-analyzer.",
        install_hint: "rustup component add rust-analyzer",
    },
    LspServerInfo {
        server_id: "bash-language-server",
        package: "bash-language-server",
        extensions: &["sh", "bash", "zsh"],
        binaries: &["bash-language-server"],
        spawn_args: &["start"],
        description: "Shell script language server; shellcheck is recommended for diagnostics.",
        install_hint: "npm install -g bash-language-server and install shellcheck",
    },
    LspServerInfo {
        server_id: "vscode-json-language-server",
        package: "vscode-langservers-extracted",
        extensions: &["json", "jsonc"],
        binaries: &["vscode-json-language-server"],
        spawn_args: &["--stdio"],
        description: "JSON and JSONC language server diagnostics.",
        install_hint: "npm install -g vscode-langservers-extracted",
    },
    LspServerInfo {
        server_id: "yaml-language-server",
        package: "yaml-language-server",
        extensions: &["yaml", "yml"],
        binaries: &["yaml-language-server"],
        spawn_args: &["--stdio"],
        description: "YAML language server diagnostics.",
        install_hint: "npm install -g yaml-language-server",
    },
];

pub(super) fn lsp_status_report(root: &Path, installed_only: bool) -> AppResult<String> {
    lsp_reap_idle_clients();
    let clients = lsp_client_snapshots(root)?;
    let broken = lsp_broken_snapshots(root)?;
    let servers = lsp_status_entries(root)
        .into_iter()
        .filter(|entry| {
            !installed_only
                || entry
                    .get("installed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let installed = servers
        .iter()
        .filter(|entry| {
            entry
                .get("installed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    Ok(serde_json::to_string_pretty(&json!({
        "workspace": root.to_string_lossy(),
        "action": "lsp_status",
        "service": {
            "enabled": !clients.is_empty(),
            "persistentClients": true,
            "activeClients": clients.len(),
            "clients": clients,
            "broken": broken,
            "idleTimeoutSeconds": LSP_IDLE_TIMEOUT_SECONDS,
            "note": "SynthChat manages persistent LSP server processes with JSON-RPC initialize, didOpen diagnostics, broken-set tracking, and idle reap."
        },
        "installedCount": installed,
        "serverCount": servers.len(),
        "servers": servers
    }))?)
}

pub(super) fn lsp_status_entries(root: &Path) -> Vec<Value> {
    LSP_SERVERS
        .iter()
        .map(|server| {
            let binary_path = server
                .binaries
                .iter()
                .find_map(|binary| resolve_workspace_executable(root, binary));
            json!({
                "serverId": server.server_id,
                "package": server.package,
                "extensions": server.extensions,
                "binaries": server.binaries,
                "description": server.description,
                "installed": binary_path.is_some(),
                "binaryPath": binary_path,
                "workspaceDetected": lsp_workspace_detected(root, server.server_id),
                "installHint": server.install_hint
            })
        })
        .collect()
}

struct LspManagedProcess {
    process_id: String,
    server_id: String,
    workspace: PathBuf,
    binary_path: PathBuf,
    args: Vec<String>,
    started_at: String,
    last_used_at: Instant,
    last_used_at_iso: String,
    json_rpc_initialized: bool,
    initialize_result: Option<Value>,
    opened_files: HashMap<String, i64>,
    diagnostics_cache: HashMap<String, Value>,
    child: Child,
}

struct LspBrokenClient {
    server_id: String,
    workspace: PathBuf,
    reason: String,
    marked_at: String,
}

static LSP_PROCESS_REGISTRY: OnceLock<Mutex<HashMap<String, LspManagedProcess>>> = OnceLock::new();
static LSP_BROKEN_REGISTRY: OnceLock<Mutex<HashMap<String, LspBrokenClient>>> = OnceLock::new();
static LSP_BASELINE_REGISTRY: OnceLock<Mutex<HashMap<String, Vec<Value>>>> = OnceLock::new();

fn lsp_process_registry() -> &'static Mutex<HashMap<String, LspManagedProcess>> {
    LSP_PROCESS_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lsp_broken_registry() -> &'static Mutex<HashMap<String, LspBrokenClient>> {
    LSP_BROKEN_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lsp_baseline_registry() -> &'static Mutex<HashMap<String, Vec<Value>>> {
    LSP_BASELINE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn lsp_lifecycle_action(root: &Path, payload: &Value, action: &str) -> AppResult<String> {
    let normalized = action.strip_prefix("lsp_").unwrap_or(action);
    match normalized {
        "which" => {
            let server = lsp_server_from_payload(payload)?;
            Ok(serde_json::to_string_pretty(&lsp_which_payload(
                root, server,
            )?)?)
        }
        "clients" | "list" => Ok(serde_json::to_string_pretty(&json!({
            "action": "lsp_clients",
            "workspace": root.to_string_lossy(),
            "clients": lsp_client_snapshots(root)?,
            "broken": lsp_broken_snapshots(root)?,
            "idleTimeoutSeconds": LSP_IDLE_TIMEOUT_SECONDS
        }))?),
        "install" => {
            let server = lsp_server_from_payload(payload)?;
            let payload = lsp_install_action(root, &[server], payload).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "install_all" => {
            let servers = LSP_SERVERS.iter().collect::<Vec<_>>();
            let payload = lsp_install_action(root, &servers, payload).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "start" => {
            let server = lsp_server_from_payload(payload)?;
            let payload = lsp_start_client(root, server).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "diagnostics" | "did_open" => {
            let payload = lsp_diagnostics_for_file(root, payload).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "baseline" | "snapshot_baseline" => {
            let payload = lsp_snapshot_baseline_for_file(root, payload).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "clear_baseline" => {
            let payload = lsp_clear_baseline_for_payload(root, payload)?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "stop" => {
            let server = optional_lsp_server_from_payload(payload)?;
            let payload = lsp_stop_clients(root, server).await?;
            Ok(serde_json::to_string_pretty(&payload)?)
        }
        "restart" => {
            let server = optional_lsp_server_from_payload(payload)?;
            let stopped = lsp_stop_clients(root, server).await?;
            let cleared_broken = lsp_clear_broken(root, server)?;
            let started = if let Some(server) = server {
                Some(lsp_start_client(root, server).await?)
            } else {
                None
            };
            Ok(serde_json::to_string_pretty(&json!({
                "action": "lsp_restart",
                "workspace": root.to_string_lossy(),
                "stopped": stopped,
                "clearedBroken": cleared_broken,
                "started": started
            }))?)
        }
        _ => Err(AppError::BadRequest(format!(
            "unsupported workspace_diagnostics LSP action: {action}"
        ))),
    }
}

fn lsp_server_from_payload(payload: &Value) -> AppResult<&'static LspServerInfo> {
    let raw = payload
        .get("server")
        .or_else(|| payload.get("serverId"))
        .or_else(|| payload.get("server_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("LSP action requires server/serverId".into()))?;
    lsp_server_by_id(raw).ok_or_else(|| AppError::BadRequest(format!("unknown LSP server: {raw}")))
}

fn optional_lsp_server_from_payload(payload: &Value) -> AppResult<Option<&'static LspServerInfo>> {
    let Some(raw) = payload
        .get("server")
        .or_else(|| payload.get("serverId"))
        .or_else(|| payload.get("server_id"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    lsp_server_by_id(raw)
        .map(Some)
        .ok_or_else(|| AppError::BadRequest(format!("unknown LSP server: {raw}")))
}

fn lsp_server_by_id(raw: &str) -> Option<&'static LspServerInfo> {
    let wanted = raw.trim().to_ascii_lowercase().replace('_', "-");
    LSP_SERVERS
        .iter()
        .find(|server| server.server_id == wanted || server.package == wanted)
}

fn lsp_server_for_path(path: &Path) -> Option<&'static LspServerInfo> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if extension.is_empty() {
        return None;
    }
    LSP_SERVERS
        .iter()
        .find(|server| server.extensions.iter().any(|ext| *ext == extension))
}

fn lsp_registry_key(server_id: &str, root: &Path) -> String {
    format!("{}::{}", server_id, root.to_string_lossy())
}

pub(super) fn lsp_mark_broken(root: &Path, server_id: &str, reason: impl Into<String>) {
    let key = lsp_registry_key(server_id, root);
    let broken = LspBrokenClient {
        server_id: server_id.into(),
        workspace: root.to_path_buf(),
        reason: reason.into(),
        marked_at: now_iso(),
    };
    if let Ok(mut guard) = lsp_broken_registry().lock() {
        guard.insert(key, broken);
    }
}

fn lsp_is_broken(root: &Path, server_id: &str) -> AppResult<Option<Value>> {
    let key = lsp_registry_key(server_id, root);
    let guard = lsp_broken_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP broken registry lock poisoned".into()))?;
    Ok(guard.get(&key).map(lsp_broken_snapshot))
}

fn lsp_clear_broken(root: &Path, server: Option<&LspServerInfo>) -> AppResult<usize> {
    let mut guard = lsp_broken_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP broken registry lock poisoned".into()))?;
    let keys = guard
        .iter()
        .filter(|(_, broken)| {
            broken.workspace == root
                && server
                    .map(|server| broken.server_id == server.server_id)
                    .unwrap_or(true)
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let count = keys.len();
    for key in keys {
        guard.remove(&key);
    }
    Ok(count)
}

pub(super) fn lsp_clear_all_broken_for_workspace(root: &Path) -> AppResult<usize> {
    lsp_clear_broken(root, None)
}

pub(super) fn lsp_broken_snapshots(root: &Path) -> AppResult<Vec<Value>> {
    let guard = lsp_broken_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP broken registry lock poisoned".into()))?;
    Ok(guard
        .values()
        .filter(|broken| broken.workspace == root)
        .map(lsp_broken_snapshot)
        .collect())
}

fn lsp_broken_snapshot(broken: &LspBrokenClient) -> Value {
    json!({
        "serverId": broken.server_id,
        "workspace": broken.workspace.to_string_lossy(),
        "reason": broken.reason,
        "markedAt": broken.marked_at
    })
}

fn lsp_which_payload(root: &Path, server: &LspServerInfo) -> AppResult<Value> {
    let binary_path = server
        .binaries
        .iter()
        .find_map(|binary| resolve_workspace_executable(root, binary));
    Ok(json!({
        "action": "lsp_which",
        "workspace": root.to_string_lossy(),
        "serverId": server.server_id,
        "package": server.package,
        "binaries": server.binaries,
        "binaryPath": binary_path,
        "installed": binary_path.is_some(),
        "spawnArgs": server.spawn_args,
        "installHint": server.install_hint
    }))
}

fn lsp_install_recipe(server_id: &str) -> Option<LspInstallRecipe> {
    match server_id {
        "pyright" => Some(LspInstallRecipe {
            command: "npm",
            args: &["install", "-g", "pyright"],
        }),
        "typescript" => Some(LspInstallRecipe {
            command: "npm",
            args: &["install", "-g", "typescript-language-server", "typescript"],
        }),
        "gopls" => Some(LspInstallRecipe {
            command: "go",
            args: &["install", "golang.org/x/tools/gopls@latest"],
        }),
        "rust-analyzer" => Some(LspInstallRecipe {
            command: "rustup",
            args: &["component", "add", "rust-analyzer"],
        }),
        "bash-language-server" => Some(LspInstallRecipe {
            command: "npm",
            args: &["install", "-g", "bash-language-server"],
        }),
        "vscode-json-language-server" => Some(LspInstallRecipe {
            command: "npm",
            args: &["install", "-g", "vscode-langservers-extracted"],
        }),
        "yaml-language-server" => Some(LspInstallRecipe {
            command: "npm",
            args: &["install", "-g", "yaml-language-server"],
        }),
        _ => None,
    }
}

async fn lsp_install_action(
    root: &Path,
    servers: &[&LspServerInfo],
    payload: &Value,
) -> AppResult<Value> {
    let execute = payload
        .get("execute")
        .or_else(|| payload.get("run"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(240)
        .clamp(30, 900);
    let mut results = Vec::new();
    for server in servers {
        let before = lsp_which_payload(root, server)?;
        let recipe = lsp_install_recipe(server.server_id);
        let mut result = json!({
            "serverId": server.server_id,
            "package": server.package,
            "installedBefore": before["installed"].as_bool().unwrap_or(false),
            "binaryPathBefore": before["binaryPath"].clone(),
            "installHint": server.install_hint,
            "execute": execute,
            "recipe": recipe.as_ref().map(|recipe| json!({
                "command": recipe.command,
                "args": recipe.args,
                "display": recipe.display()
            }))
        });
        if let Some(recipe) = recipe {
            if execute {
                let output = run_lsp_install_recipe(root, &recipe, timeout_seconds).await;
                match output {
                    Ok(value) => {
                        result["ok"] = value["ok"].clone();
                        result["exitCode"] = value["exitCode"].clone();
                        result["stdout"] = value["stdout"].clone();
                        result["stderr"] = value["stderr"].clone();
                        result["timedOut"] = value["timedOut"].clone();
                        result["installedAfter"] =
                            lsp_which_payload(root, server)?["installed"].clone();
                    }
                    Err(error) => {
                        result["ok"] = json!(false);
                        result["error"] = json!(error.to_string());
                    }
                }
            } else {
                result["ok"] = json!(true);
                result["dryRun"] = json!(true);
            }
        } else {
            result["ok"] = json!(false);
            result["error"] = json!("no install recipe is registered for this server");
        }
        results.push(result);
    }
    Ok(json!({
        "action": if servers.len() == 1 { "lsp_install" } else { "lsp_install_all" },
        "workspace": root.to_string_lossy(),
        "execute": execute,
        "timeoutSeconds": timeout_seconds,
        "results": results
    }))
}

async fn run_lsp_install_recipe(
    root: &Path,
    recipe: &LspInstallRecipe,
    timeout_seconds: u64,
) -> AppResult<Value> {
    let mut command = lsp_install_command(recipe);
    command.hide_window();
    command
        .args(recipe.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(timeout_seconds), command.output())
        .await
        .map_err(|_| {
            AppError::BadRequest(format!("LSP install timed out: {}", recipe.display()))
        })??;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(json!({
        "ok": output.status.success(),
        "exitCode": output.status.code(),
        "timedOut": false,
        "stdout": truncate_output(&stdout, 4000),
        "stderr": truncate_output(&stderr, 4000)
    }))
}

fn lsp_install_command(recipe: &LspInstallRecipe) -> Command {
    #[cfg(windows)]
    {
        if matches!(recipe.command, "npm" | "npx" | "rustup") {
            let mut command = Command::new("cmd.exe");
            command.arg("/c").arg(recipe.command);
            return command;
        }
    }
    Command::new(recipe.command)
}

async fn lsp_start_client(root: &Path, server: &LspServerInfo) -> AppResult<Value> {
    lsp_reap_idle_clients();
    if let Some(broken) = lsp_is_broken(root, server.server_id)? {
        return Err(AppError::BadRequest(format!(
            "LSP server '{}' for workspace '{}' is marked broken: {}",
            server.server_id,
            root.to_string_lossy(),
            broken
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )));
    }
    let binary_path = server
        .binaries
        .iter()
        .find_map(|binary| resolve_workspace_executable(root, binary))
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "LSP server '{}' is not installed; {}",
                server.server_id, server.install_hint
            ))
        })
        .map(PathBuf::from)?;
    let key = lsp_registry_key(server.server_id, root);
    {
        let registry = lsp_process_registry();
        let mut guard = registry
            .lock()
            .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
        if let Some(existing) = guard.get_mut(&key) {
            if existing.child.try_wait()?.is_none() {
                lsp_touch_process(existing);
                return Ok(lsp_process_snapshot(existing, "already_running"));
            }
        }
        guard.remove(&key);
    }

    let mut command = lsp_spawn_command(&binary_path);
    command.hide_window();
    command
        .args(server.spawn_args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to start LSP server {}: {error}", server.server_id);
            lsp_mark_broken(root, server.server_id, message.clone());
            return Err(AppError::BadRequest(message));
        }
    };
    let initialize_result = match lsp_initialize_child(&mut child, root, server).await {
        Ok(result) => result,
        Err(error) => {
            lsp_mark_broken(root, server.server_id, error.to_string());
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            return Err(error);
        }
    };
    let process_id = child
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| new_id("lsp"));
    let mut managed = LspManagedProcess {
        process_id,
        server_id: server.server_id.into(),
        workspace: root.to_path_buf(),
        binary_path,
        args: server
            .spawn_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect(),
        started_at: now_iso(),
        last_used_at: Instant::now(),
        last_used_at_iso: now_iso(),
        json_rpc_initialized: true,
        initialize_result: Some(initialize_result),
        opened_files: HashMap::new(),
        diagnostics_cache: HashMap::new(),
        child,
    };
    let snapshot = lsp_process_snapshot(&mut managed, "started");
    let registry = lsp_process_registry();
    let mut guard = registry
        .lock()
        .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
    guard.insert(key, managed);
    Ok(snapshot)
}

pub(super) fn lsp_snapshot_baseline_blocking(root: &Path, file_path: &Path) -> Value {
    lsp_blocking_file_action(root, file_path, "lsp_snapshot_baseline")
}

pub(super) fn lsp_delta_diagnostics_blocking(root: &Path, file_path: &Path) -> Value {
    lsp_blocking_file_action(root, file_path, "lsp_diagnostics")
}

pub(super) fn lsp_clear_baseline_for_path(root: &Path, file_path: &Path) -> Value {
    let cleared = lsp_baseline_registry()
        .lock()
        .map(|mut guard| {
            guard
                .remove(&lsp_baseline_key(file_path))
                .map(|_| 1)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    json!({
        "action": "lsp_clear_baseline",
        "workspace": root.to_string_lossy(),
        "file": file_path.to_string_lossy(),
        "cleared": cleared
    })
}

fn lsp_blocking_file_action(root: &Path, file_path: &Path, action: &str) -> Value {
    if !file_path.is_file() {
        return json!({
            "action": action,
            "enabled": false,
            "reason": "path is not a file",
            "file": file_path.to_string_lossy()
        });
    }
    let payload = json!({
        "action": action,
        "workspaceDir": ".",
        "path": file_path.to_string_lossy(),
        "timeoutSeconds": LSP_DIAGNOSTICS_WAIT_SECONDS,
        "delta": action == "lsp_diagnostics"
    });
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return json!({
            "action": action,
            "enabled": false,
            "reason": "no async runtime available",
            "file": file_path.to_string_lossy()
        });
    };
    let result = tokio::task::block_in_place(|| {
        handle.block_on(async {
            match action {
                "lsp_snapshot_baseline" => lsp_snapshot_baseline_for_file(root, &payload).await,
                "lsp_diagnostics" => lsp_diagnostics_for_file(root, &payload).await,
                _ => Err(AppError::BadRequest(format!(
                    "unsupported LSP file action: {action}"
                ))),
            }
        })
    });
    match result {
        Ok(value) => value,
        Err(error) => json!({
            "action": action,
            "enabled": false,
            "reason": error.to_string(),
            "file": file_path.to_string_lossy()
        }),
    }
}

async fn lsp_diagnostics_for_file(root: &Path, payload: &Value) -> AppResult<Value> {
    lsp_reap_idle_clients();
    let file_path = lsp_file_path_from_payload(root, payload)?;
    let server = optional_lsp_server_from_payload(payload)?
        .or_else(|| lsp_server_for_path(&file_path))
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "could not infer LSP server for file extension: {}",
                file_path.to_string_lossy()
            ))
        })?;
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(LSP_DIAGNOSTICS_WAIT_SECONDS)
        .clamp(1, 60);
    let delta = payload
        .get("delta")
        .or_else(|| payload.get("onlyNew"))
        .or_else(|| payload.get("only_new"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let key = lsp_registry_key(server.server_id, root);
    let mut process = take_lsp_process(root, server).await?;
    let result = lsp_send_did_open_and_collect(&mut process, &file_path, timeout_seconds).await;
    match result {
        Ok(mut payload) => {
            if delta && !payload["timedOut"].as_bool().unwrap_or(false) {
                let full = payload["diagnostics"].clone();
                let filtered = lsp_filter_delta_and_roll_baseline(&file_path, &full)?;
                payload["diagnostics"] = filtered["diagnostics"].clone();
                payload["diagnosticsText"] = json!(lsp_diagnostics_text(&filtered["diagnostics"]));
                payload["delta"] = json!(true);
                payload["baseline"] = filtered["baseline"].clone();
            } else {
                payload["delta"] = json!(false);
                payload["baseline"] = lsp_baseline_status_for_file(&file_path)?;
            }
            payload["serverId"] = json!(server.server_id);
            payload["workspace"] = json!(root.to_string_lossy());
            payload["client"] = lsp_process_snapshot(&mut process, "running");
            let registry = lsp_process_registry();
            let mut guard = registry
                .lock()
                .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
            guard.insert(key, process);
            Ok(payload)
        }
        Err(error) => {
            lsp_mark_broken(root, server.server_id, error.to_string());
            let _ = process.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), process.child.wait()).await;
            Err(error)
        }
    }
}

async fn lsp_snapshot_baseline_for_file(root: &Path, payload: &Value) -> AppResult<Value> {
    let file_path = lsp_file_path_from_payload(root, payload)?;
    let mut diagnostics_payload = lsp_diagnostics_for_file(
        root,
        &json!({
            "path": file_path.to_string_lossy(),
            "server": payload.get("server").or_else(|| payload.get("serverId")).or_else(|| payload.get("server_id")).cloned().unwrap_or(Value::Null),
            "timeoutSeconds": payload.get("timeoutSeconds").or_else(|| payload.get("timeout_seconds")).cloned().unwrap_or(json!(LSP_DIAGNOSTICS_WAIT_SECONDS)),
            "delta": false
        }),
    )
    .await?;
    let diagnostics = diagnostics_payload["diagnostics"].clone();
    let baseline_count = lsp_store_baseline(
        &file_path,
        diagnostics.as_array().cloned().unwrap_or_default(),
    )?;
    diagnostics_payload["action"] = json!("lsp_snapshot_baseline");
    diagnostics_payload["baseline"] = json!({
        "stored": true,
        "count": baseline_count,
        "file": file_path.to_string_lossy()
    });
    Ok(diagnostics_payload)
}

fn lsp_clear_baseline_for_payload(root: &Path, payload: &Value) -> AppResult<Value> {
    let file_path = payload
        .get("path")
        .or_else(|| payload.get("file"))
        .or_else(|| payload.get("filePath"))
        .or_else(|| payload.get("file_path"))
        .and_then(Value::as_str)
        .map(|_| lsp_file_path_from_payload(root, payload))
        .transpose()?;
    let mut guard = lsp_baseline_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP baseline registry lock poisoned".into()))?;
    let cleared = if let Some(file_path) = file_path.as_ref() {
        guard
            .remove(&lsp_baseline_key(&file_path))
            .map(|_| 1)
            .unwrap_or(0)
    } else {
        let keys = guard
            .keys()
            .filter(|key| key.starts_with(&root.to_string_lossy().to_string()))
            .cloned()
            .collect::<Vec<_>>();
        let count = keys.len();
        for key in keys {
            guard.remove(&key);
        }
        count
    };
    Ok(json!({
        "action": "lsp_clear_baseline",
        "workspace": root.to_string_lossy(),
        "file": file_path.map(|path| path.to_string_lossy().to_string()),
        "cleared": cleared
    }))
}

fn lsp_file_path_from_payload(root: &Path, payload: &Value) -> AppResult<PathBuf> {
    let raw_path = payload
        .get("path")
        .or_else(|| payload.get("file"))
        .or_else(|| payload.get("filePath"))
        .or_else(|| payload.get("file_path"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("LSP diagnostics action requires path/file".into()))?;
    let file_path = resolve_workspace_path(root, raw_path)?;
    if !file_path.is_file() {
        return Err(AppError::BadRequest(format!(
            "LSP diagnostics path is not a file: {}",
            file_path.to_string_lossy()
        )));
    }
    Ok(file_path)
}

async fn take_lsp_process(root: &Path, server: &LspServerInfo) -> AppResult<LspManagedProcess> {
    let key = lsp_registry_key(server.server_id, root);
    let existing = {
        let registry = lsp_process_registry();
        let mut guard = registry
            .lock()
            .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
        guard.remove(&key)
    };
    if let Some(mut process) = existing {
        if process.child.try_wait()?.is_none() {
            lsp_touch_process(&mut process);
            return Ok(process);
        }
    }
    let _ = lsp_start_client(root, server).await?;
    let registry = lsp_process_registry();
    let mut guard = registry
        .lock()
        .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
    guard
        .remove(&key)
        .ok_or_else(|| AppError::BadRequest("LSP client was not registered after start".into()))
}

fn lsp_touch_process(process: &mut LspManagedProcess) {
    process.last_used_at = Instant::now();
    process.last_used_at_iso = now_iso();
}

async fn lsp_send_did_open_and_collect(
    process: &mut LspManagedProcess,
    file_path: &Path,
    timeout_seconds: u64,
) -> AppResult<Value> {
    if let Some(exit) = process.child.try_wait()? {
        return Err(AppError::BadRequest(format!(
            "LSP server {} is not running: exited({})",
            process.server_id,
            exit.code().unwrap_or(-1)
        )));
    }
    let text = fs::read_to_string(file_path).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read file for LSP didOpen {}: {error}",
            file_path.to_string_lossy()
        ))
    })?;
    let uri = lsp_file_uri(file_path);
    let language_id = lsp_language_id_for_path(file_path);
    let stdin = process
        .child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::BadRequest("LSP child stdin is unavailable".into()))?;
    let version = process.opened_files.get(&uri).copied().unwrap_or(0) + 1;
    if version == 1 {
        lsp_write_message(
            stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
                        "version": version,
                        "text": text
                    }
                }
            }),
        )
        .await?;
    } else {
        lsp_write_message(
            stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "version": version
                    },
                    "contentChanges": [{
                        "text": text
                    }]
                }
            }),
        )
        .await?;
    }
    process.opened_files.insert(uri.clone(), version);
    lsp_write_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": {
                "changes": [{
                    "uri": uri,
                    "type": 1
                }]
            }
        }),
    )
    .await?;
    lsp_write_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {
                "textDocument": {
                    "uri": uri
                },
                "text": text
            }
        }),
    )
    .await?;
    let stdout = process
        .child
        .stdout
        .as_mut()
        .ok_or_else(|| AppError::BadRequest("LSP child stdout is unavailable".into()))?;
    let started = Instant::now();
    loop {
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(timeout_seconds) {
            let cached = process
                .diagnostics_cache
                .get(&uri)
                .cloned()
                .unwrap_or_else(|| Value::Array(vec![]));
            return Ok(lsp_diagnostics_payload(
                file_path, &uri, cached, true, true, version,
            ));
        }
        let remaining = Duration::from_secs(timeout_seconds).saturating_sub(elapsed);
        let message = match tokio::time::timeout(remaining, lsp_read_message(stdout)).await {
            Ok(result) => result?,
            Err(_) => {
                let cached = process
                    .diagnostics_cache
                    .get(&uri)
                    .cloned()
                    .unwrap_or_else(|| Value::Array(vec![]));
                return Ok(lsp_diagnostics_payload(
                    file_path, &uri, cached, true, true, version,
                ));
            }
        };
        let Some(message) = message else {
            return Err(AppError::BadRequest(format!(
                "LSP server {} closed stdout while waiting for diagnostics",
                process.server_id
            )));
        };
        if message.get("method").is_some() && message.get("id").is_some() {
            let response = lsp_response_for_server_request(&message);
            let stdin = process
                .child
                .stdin
                .as_mut()
                .ok_or_else(|| AppError::BadRequest("LSP child stdin is unavailable".into()))?;
            lsp_write_message(stdin, &response).await?;
            continue;
        }
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if params.get("uri").and_then(Value::as_str) == Some(uri.as_str()) {
                let diagnostics = params
                    .get("diagnostics")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(vec![]));
                process
                    .diagnostics_cache
                    .insert(uri.clone(), diagnostics.clone());
                return Ok(lsp_diagnostics_payload(
                    file_path,
                    &uri,
                    diagnostics,
                    false,
                    false,
                    version,
                ));
            }
        }
    }
}

fn lsp_diagnostics_payload(
    file_path: &Path,
    uri: &str,
    diagnostics: Value,
    timed_out: bool,
    from_cache: bool,
    version: i64,
) -> Value {
    json!({
        "action": "lsp_diagnostics",
        "file": file_path.to_string_lossy(),
        "uri": uri,
        "languageId": lsp_language_id_for_path(file_path),
        "version": version,
        "timedOut": timed_out,
        "fromCache": from_cache,
        "diagnostics": diagnostics,
        "diagnosticsText": lsp_diagnostics_text(&diagnostics)
    })
}

fn lsp_diagnostics_text(diagnostics: &Value) -> String {
    let Some(items) = diagnostics.as_array() else {
        return String::new();
    };
    if items.is_empty() {
        return "No LSP diagnostics reported.".into();
    }
    items
        .iter()
        .take(LSP_MAX_PER_FILE)
        .map(|item| {
            let range = item.get("range").unwrap_or(&Value::Null);
            let start = range.get("start").unwrap_or(&Value::Null);
            let line = start.get("line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let column = start.get("character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let severity = item
                .get("severity")
                .and_then(Value::as_u64)
                .map(lsp_severity_name)
                .unwrap_or("INFO");
            let message = item
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace('\n', " ");
            let source = item.get("source").and_then(Value::as_str).unwrap_or("lsp");
            format!("{severity} [{line}:{column}] {message} ({source})")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lsp_filter_delta_and_roll_baseline(file_path: &Path, diagnostics: &Value) -> AppResult<Value> {
    let full = diagnostics.as_array().cloned().unwrap_or_default();
    let key = lsp_baseline_key(file_path);
    let mut guard = lsp_baseline_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP baseline registry lock poisoned".into()))?;
    let baseline = guard.get(&key).cloned().unwrap_or_default();
    let baseline_keys = baseline
        .iter()
        .map(lsp_diagnostic_key)
        .collect::<BTreeSet<_>>();
    let filtered = if baseline_keys.is_empty() {
        full.clone()
    } else {
        full.iter()
            .filter(|diagnostic| !baseline_keys.contains(&lsp_diagnostic_key(diagnostic)))
            .cloned()
            .collect::<Vec<_>>()
    };
    guard.insert(key, full.clone());
    Ok(json!({
        "diagnostics": filtered,
        "baseline": {
            "applied": !baseline_keys.is_empty(),
            "previousCount": baseline.len(),
            "currentCount": full.len(),
            "filteredCount": full.len().saturating_sub(filtered.len()),
            "rolledForward": true,
            "file": file_path.to_string_lossy()
        }
    }))
}

fn lsp_store_baseline(file_path: &Path, diagnostics: Vec<Value>) -> AppResult<usize> {
    let count = diagnostics.len();
    let mut guard = lsp_baseline_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP baseline registry lock poisoned".into()))?;
    guard.insert(lsp_baseline_key(file_path), diagnostics);
    Ok(count)
}

fn lsp_baseline_status_for_file(file_path: &Path) -> AppResult<Value> {
    let guard = lsp_baseline_registry()
        .lock()
        .map_err(|_| AppError::BadRequest("LSP baseline registry lock poisoned".into()))?;
    let count = guard
        .get(&lsp_baseline_key(file_path))
        .map(|diagnostics| diagnostics.len())
        .unwrap_or(0);
    Ok(json!({
        "applied": false,
        "previousCount": count,
        "rolledForward": false,
        "file": file_path.to_string_lossy()
    }))
}

fn lsp_baseline_key(file_path: &Path) -> String {
    file_path.to_string_lossy().replace('\\', "/")
}

pub(super) fn lsp_diagnostic_key(diagnostic: &Value) -> String {
    let range = diagnostic.get("range").unwrap_or(&Value::Null);
    let start = range.get("start").unwrap_or(&Value::Null);
    let end = range.get("end").unwrap_or(&Value::Null);
    let code = diagnostic
        .get("code")
        .map(|code| {
            code.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| code.to_string())
        })
        .unwrap_or_default();
    [
        diagnostic
            .get("severity")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .to_string(),
        code,
        diagnostic
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        diagnostic
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        format!(
            "{}:{}-{}:{}",
            start.get("line").and_then(Value::as_u64).unwrap_or(0),
            start.get("character").and_then(Value::as_u64).unwrap_or(0),
            end.get("line").and_then(Value::as_u64).unwrap_or(0),
            end.get("character").and_then(Value::as_u64).unwrap_or(0)
        ),
    ]
    .join("\u{0}")
}

fn lsp_severity_name(value: u64) -> &'static str {
    match value {
        1 => "ERROR",
        2 => "WARNING",
        3 => "INFO",
        4 => "HINT",
        _ => "INFO",
    }
}

fn lsp_spawn_command(binary_path: &Path) -> Command {
    #[cfg(windows)]
    {
        let extension = binary_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "cmd" | "bat") {
            let mut command = Command::new("cmd.exe");
            command.arg("/c").arg(binary_path);
            return command;
        }
    }
    Command::new(binary_path)
}

async fn lsp_initialize_child(
    child: &mut Child,
    root: &Path,
    server: &LspServerInfo,
) -> AppResult<Value> {
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::BadRequest("LSP child stdin was not piped".into()))?;
    let stdout = child
        .stdout
        .as_mut()
        .ok_or_else(|| AppError::BadRequest("LSP child stdout was not piped".into()))?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "clientInfo": {
                "name": "SynthChat",
                "version": env!("CARGO_PKG_VERSION")
            },
            "rootPath": root.to_string_lossy(),
            "rootUri": lsp_file_uri(root),
            "workspaceFolders": [{
                "name": root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
                "uri": lsp_file_uri(root)
            }],
            "capabilities": {
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true
                },
                "textDocument": {
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": true
                    }
                }
            },
            "trace": "off"
        }
    });
    lsp_write_message(stdin, &request).await?;
    let initialize_result = tokio::time::timeout(
        Duration::from_secs(LSP_INITIALIZE_TIMEOUT_SECONDS),
        lsp_wait_for_initialize_response(stdin, stdout, server),
    )
    .await
    .map_err(|_| {
        AppError::BadRequest(format!(
            "LSP server {} did not complete initialize within {}s",
            server.server_id, LSP_INITIALIZE_TIMEOUT_SECONDS
        ))
    })??;
    lsp_write_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    )
    .await?;
    Ok(initialize_result)
}

async fn lsp_wait_for_initialize_response<W, R>(
    writer: &mut W,
    reader: &mut R,
    server: &LspServerInfo,
) -> AppResult<Value>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    loop {
        let Some(message) = lsp_read_message(reader).await? else {
            return Err(AppError::BadRequest(format!(
                "LSP server {} closed stdout before initialize response",
                server.server_id
            )));
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(AppError::BadRequest(format!(
                "LSP server {} sent non JSON-RPC 2.0 message",
                server.server_id
            )));
        }
        if message.get("id").and_then(Value::as_i64) == Some(1)
            || message.get("id").and_then(Value::as_u64) == Some(1)
        {
            if let Some(error) = message.get("error") {
                return Err(AppError::BadRequest(format!(
                    "LSP server {} initialize failed: {}",
                    server.server_id, error
                )));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
        if message.get("method").is_some() && message.get("id").is_some() {
            let response = lsp_response_for_server_request(&message);
            lsp_write_message(writer, &response).await?;
        }
    }
}

fn lsp_response_for_server_request(message: &Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "workspace/diagnostic/refresh" => Value::Null,
        "workspace/workspaceFolders" => Value::Null,
        "workspace/configuration" => {
            let count = message
                .get("params")
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0);
            Value::Array((0..count).map(|_| json!({})).collect())
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("method not found: {method}")
                }
            });
        }
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

async fn lsp_write_message<W>(writer: &mut W, message: &Value) -> AppResult<()>
where
    W: AsyncWrite + Unpin,
{
    let encoded = lsp_encode_message(message)?;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) fn lsp_encode_message(message: &Value) -> AppResult<Vec<u8>> {
    let body = serde_json::to_vec(message)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut encoded = header.into_bytes();
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

pub(super) async fn lsp_read_message<R>(reader: &mut R) -> AppResult<Option<Value>>
where
    R: AsyncRead + Unpin,
{
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            if headers.is_empty() {
                return Ok(None);
            }
            return Err(AppError::BadRequest(
                "unexpected EOF while reading LSP headers".into(),
            ));
        }
        headers.push(byte[0]);
        if headers.len() > LSP_MAX_HEADER_BYTES {
            return Err(AppError::BadRequest(
                "LSP header block exceeded 8 KiB".into(),
            ));
        }
    }
    let header_text = std::str::from_utf8(&headers)
        .map_err(|error| AppError::BadRequest(format!("non-UTF8 LSP headers: {error}")))?;
    let mut content_length = None;
    for line in header_text.split("\r\n") {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(AppError::BadRequest(format!(
                "malformed LSP header line: {line}"
            )));
        };
        if key.trim().eq_ignore_ascii_case("Content-Length") {
            let parsed = value.trim().parse::<usize>().map_err(|error| {
                AppError::BadRequest(format!("invalid LSP Content-Length: {error}"))
            })?;
            if parsed > LSP_MAX_BODY_BYTES {
                return Err(AppError::BadRequest(format!(
                    "LSP Content-Length exceeds limit: {parsed}"
                )));
            }
            content_length = Some(parsed);
        }
    }
    let length = content_length
        .ok_or_else(|| AppError::BadRequest("LSP message missing Content-Length".into()))?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    let value = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid LSP JSON body: {error}")))?;
    Ok(Some(value))
}

pub(super) fn lsp_file_uri(path: &Path) -> String {
    let mut raw = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        if raw.len() > 1 && raw.as_bytes().get(1) == Some(&b':') {
            raw.insert(0, '/');
        }
    }
    format!("file://{}", lsp_uri_escape_path(&raw))
}

pub(super) fn lsp_language_id_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "go" => "go",
        "rs" => "rust",
        "sh" | "bash" | "zsh" => "shellscript",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        _ => "plaintext",
    }
}

fn lsp_uri_escape_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | ':' | '-' | '_' | '.' | '~') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

async fn lsp_stop_clients(root: &Path, server: Option<&LspServerInfo>) -> AppResult<Value> {
    lsp_reap_idle_clients();
    let removed = {
        let registry = lsp_process_registry();
        let mut guard = registry
            .lock()
            .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
        let keys = guard
            .iter()
            .filter(|(_, process)| {
                process.workspace == root
                    && server
                        .map(|server| process.server_id == server.server_id)
                        .unwrap_or(true)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| guard.remove(&key))
            .collect::<Vec<_>>()
    };

    let mut stopped = Vec::new();
    for mut process in removed {
        let _ = process.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(3), process.child.wait()).await;
        stopped.push(json!({
            "processId": process.process_id,
            "serverId": process.server_id,
            "workspace": process.workspace.to_string_lossy(),
            "binaryPath": process.binary_path,
            "startedAt": process.started_at
        }));
    }
    let cleared_broken = lsp_clear_broken(root, server)?;
    Ok(json!({
        "action": "lsp_stop",
        "workspace": root.to_string_lossy(),
        "serverId": server.map(|server| server.server_id),
        "stoppedCount": stopped.len(),
        "clearedBroken": cleared_broken,
        "stopped": stopped
    }))
}

fn lsp_client_snapshots(root: &Path) -> AppResult<Vec<Value>> {
    lsp_reap_idle_clients();
    let registry = lsp_process_registry();
    let mut guard = registry
        .lock()
        .map_err(|_| AppError::BadRequest("LSP process registry lock poisoned".into()))?;
    let mut stale = Vec::new();
    let mut snapshots = Vec::new();
    for (key, process) in guard.iter_mut() {
        if process.workspace != root {
            continue;
        }
        let status = match process.child.try_wait()? {
            Some(exit) => {
                stale.push(key.clone());
                format!("exited({})", exit.code().unwrap_or(-1))
            }
            None => "running".into(),
        };
        snapshots.push(lsp_process_snapshot(process, &status));
    }
    for key in stale {
        guard.remove(&key);
    }
    Ok(snapshots)
}

pub(super) fn lsp_reap_idle_clients() -> Vec<Value> {
    let mut reaped = Vec::new();
    let Ok(mut guard) = lsp_process_registry().lock() else {
        return reaped;
    };
    let now = Instant::now();
    let keys = guard
        .iter_mut()
        .filter_map(|(key, process)| {
            let exited = process
                .child
                .try_wait()
                .ok()
                .flatten()
                .map(|exit| format!("exited({})", exit.code().unwrap_or(-1)));
            let idle_for = now.saturating_duration_since(process.last_used_at);
            if let Some(status) = exited {
                return Some((key.clone(), status));
            }
            if idle_for >= Duration::from_secs(LSP_IDLE_TIMEOUT_SECONDS) {
                return Some((key.clone(), format!("idle({}s)", idle_for.as_secs())));
            }
            None
        })
        .collect::<Vec<_>>();
    for (key, reason) in keys {
        if let Some(mut process) = guard.remove(&key) {
            let _ = process.child.start_kill();
            reaped.push(json!({
                "serverId": process.server_id,
                "workspace": process.workspace.to_string_lossy(),
                "processId": process.process_id,
                "reason": reason,
                "lastUsedAt": process.last_used_at_iso,
                "startedAt": process.started_at
            }));
        }
    }
    reaped
}

fn lsp_process_snapshot(process: &mut LspManagedProcess, status: &str) -> Value {
    let idle_seconds = Instant::now()
        .saturating_duration_since(process.last_used_at)
        .as_secs();
    json!({
        "action": "lsp_client",
        "status": status,
        "serverId": process.server_id,
        "workspace": process.workspace.to_string_lossy(),
        "processId": process.process_id,
        "pid": process.child.id(),
        "binaryPath": process.binary_path,
        "args": process.args,
        "startedAt": process.started_at,
        "lastUsedAt": process.last_used_at_iso,
        "idleSeconds": idle_seconds,
        "idleTimeoutSeconds": LSP_IDLE_TIMEOUT_SECONDS,
        "openedFiles": process.opened_files.len(),
        "diagnosticsCachedFiles": process.diagnostics_cache.len(),
        "persistentClient": true,
        "jsonRpcInitialized": process.json_rpc_initialized,
        "initializeResult": process.initialize_result,
        "note": if process.json_rpc_initialized {
            "Process lifecycle is managed and JSON-RPC initialize completed; didOpen diagnostics are available via workspace_diagnostics action=lsp_diagnostics."
        } else {
            "Process lifecycle is managed; JSON-RPC initialize did not complete."
        }
    })
}

fn lsp_workspace_detected(root: &Path, server_id: &str) -> bool {
    match server_id {
        "pyright" => python_workspace_detected(root),
        "typescript" => root.join("tsconfig.json").exists() || root.join("package.json").exists(),
        "gopls" => go_workspace_detected(root),
        "rust-analyzer" => root.join("Cargo.toml").exists(),
        "bash-language-server" => workspace_has_extension(root, &["sh", "bash", "zsh"]),
        "vscode-json-language-server" => workspace_has_extension(root, &["json", "jsonc"]),
        "yaml-language-server" => workspace_has_extension(root, &["yaml", "yml"]),
        _ => false,
    }
}

fn workspace_has_extension(root: &Path, extensions: &[&str]) -> bool {
    fs::read_dir(root).ok().into_iter().flatten().any(|entry| {
        entry
            .ok()
            .and_then(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| {
                        extensions
                            .iter()
                            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
                    })
            })
            .unwrap_or(false)
    })
}

fn diagnostics_workspace(agent: &AgentDefinition, payload: &Value) -> AppResult<PathBuf> {
    let root = workspace_root(agent)?;
    let input = payload
        .get("workspaceDir")
        .or_else(|| payload.get("workspace_dir"))
        .or_else(|| payload.get("cwd"))
        .or_else(|| payload.get("root"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let path = resolve_workspace_path(&root, input)?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(AppError::BadRequest(format!(
            "workspace_diagnostics requires a directory: {}",
            path.display()
        )))
    }
}

pub(super) fn diagnostics_mode(payload: &Value) -> String {
    match payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "rust" | "rs" | "cargo" => "rust".into(),
        "typescript" | "typecheck" | "tsc" | "ts" => "typescript".into(),
        "python" | "py" | "pyright" => "python".into(),
        "go" | "golang" | "gopls" => "go".into(),
        "all" => "all".into(),
        _ => "auto".into(),
    }
}

pub(super) fn diagnostic_commands_for_workspace(root: &Path, mode: &str) -> Vec<DiagnosticCommand> {
    let mut commands = Vec::new();
    if matches!(mode, "auto" | "all" | "rust") && root.join("Cargo.toml").exists() {
        commands.push(DiagnosticCommand {
            family: "rust",
            program: platform_command("cargo"),
            args: vec!["check".into(), "--tests".into()],
            display: "cargo check --tests".into(),
        });
    }
    if matches!(mode, "auto" | "all" | "typescript") && root.join("tsconfig.json").exists() {
        commands.push(DiagnosticCommand {
            family: "typescript",
            program: platform_command("npx"),
            args: vec!["--no-install".into(), "tsc".into(), "--noEmit".into()],
            display: "npx --no-install tsc --noEmit".into(),
        });
    }
    if matches!(mode, "auto" | "all" | "go") && go_workspace_detected(root) {
        commands.push(DiagnosticCommand {
            family: "go",
            program: platform_command("go"),
            args: vec!["test".into(), "./...".into()],
            display: "go test ./...".into(),
        });
    }
    if matches!(mode, "auto" | "all" | "python") && python_workspace_detected(root) {
        commands.push(python_diagnostic_command(root));
    }
    commands
}

pub(super) fn go_workspace_detected(root: &Path) -> bool {
    root.join("go.mod").exists()
        || root.join("go.work").exists()
        || fs::read_dir(root).ok().into_iter().flatten().any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.path().extension().map(|ext| ext == "go"))
                .unwrap_or(false)
        })
}

pub(super) fn python_workspace_detected(root: &Path) -> bool {
    [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "Pipfile",
        "poetry.lock",
        "pyrightconfig.json",
        ".python-version",
    ]
    .iter()
    .any(|name| root.join(name).exists())
        || root.join("src").is_dir()
            && fs::read_dir(root.join("src"))
                .ok()
                .into_iter()
                .flatten()
                .any(|entry| {
                    entry
                        .ok()
                        .and_then(|entry| entry.path().extension().map(|ext| ext == "py"))
                        .unwrap_or(false)
                })
}

fn python_diagnostic_command(root: &Path) -> DiagnosticCommand {
    if let Some(pyright) = resolve_workspace_executable(root, "pyright") {
        return DiagnosticCommand {
            family: "python_pyright",
            program: pyright,
            args: vec!["--outputjson".into()],
            display: "pyright --outputjson".into(),
        };
    }
    let python = resolve_python_executable(root).unwrap_or_else(|| platform_command("python"));
    DiagnosticCommand {
        family: "python_compileall",
        program: python,
        args: vec!["-m".into(), "compileall".into(), "-q".into(), ".".into()],
        display: "python -m compileall -q .".into(),
    }
}

fn resolve_python_executable(root: &Path) -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        candidates.push(PathBuf::from(venv));
    }
    candidates.push(root.join(".venv"));
    candidates.push(root.join("venv"));
    for venv in candidates {
        for relative in ["Scripts/python.exe", "bin/python", "bin/python3"] {
            let path = venv.join(relative);
            if path.exists() {
                return Some(path.to_string_lossy().to_string());
            }
        }
    }
    resolve_workspace_executable(root, "python")
}

fn resolve_workspace_executable(root: &Path, name: &str) -> Option<String> {
    let mut names = vec![name.to_string()];
    if cfg!(windows) {
        names.push(format!("{name}.cmd"));
        names.push(format!("{name}.exe"));
    }
    for candidate in &names {
        let local = root.join("node_modules").join(".bin").join(candidate);
        if local.exists() {
            return Some(local.to_string_lossy().to_string());
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for candidate in &names {
            let full = dir.join(candidate);
            if full.exists() {
                return Some(full.to_string_lossy().to_string());
            }
        }
    }
    None
}

pub(super) fn parse_command_diagnostics(
    family: &str,
    stdout: &str,
    stderr: &str,
) -> Vec<ParsedDiagnostic> {
    let combined = if stdout.trim().is_empty() {
        stderr.to_string()
    } else if stderr.trim().is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };
    match family {
        "rust" => parse_rust_diagnostics(&combined),
        "typescript" => parse_typescript_diagnostics(&combined),
        "go" => parse_go_diagnostics(&combined),
        "python_pyright" => parse_pyright_diagnostics(&combined),
        "python_compileall" => parse_python_compileall_diagnostics(&combined),
        _ => Vec::new(),
    }
}

pub(super) fn parse_rust_diagnostics(output: &str) -> Vec<ParsedDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut pending: Option<(String, Option<String>, String)> = None;
    for line in output.lines() {
        let trimmed = strip_ansi_codes(line).trim().to_string();
        if let Some((severity, rest)) = trimmed
            .strip_prefix("error")
            .map(|rest| ("ERROR".to_string(), rest))
            .or_else(|| {
                trimmed
                    .strip_prefix("warning")
                    .map(|rest| ("WARN".to_string(), rest))
            })
        {
            let (code, message) = parse_rust_diagnostic_header(rest);
            pending = Some((severity, code, message));
            continue;
        }
        let Some(location) = trimmed.strip_prefix("-->") else {
            continue;
        };
        let Some((file, line_no, col_no)) = parse_file_line_col(location.trim()) else {
            continue;
        };
        let (severity, code, message) = pending
            .take()
            .unwrap_or_else(|| ("ERROR".to_string(), None, "Rust diagnostic".into()));
        diagnostics.push(ParsedDiagnostic {
            file,
            line: line_no,
            column: col_no,
            severity,
            code,
            message,
            source: "rustc".into(),
        });
    }
    diagnostics
}

fn parse_rust_diagnostic_header(rest: &str) -> (Option<String>, String) {
    let mut text = rest.trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace());
    let mut code = None;
    if let Some(after_open) = text.strip_prefix('[') {
        if let Some(end) = after_open.find(']') {
            let candidate = after_open[..end].trim();
            if !candidate.is_empty() {
                code = Some(candidate.to_string());
            }
            text = after_open[end + 1..]
                .trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace());
        }
    }
    let message = if text.is_empty() {
        "Rust diagnostic".into()
    } else {
        text.to_string()
    };
    (code, message)
}

pub(super) fn parse_typescript_diagnostics(output: &str) -> Vec<ParsedDiagnostic> {
    let mut diagnostics = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi_codes(raw);
        let Some(paren_start) = line.find('(') else {
            continue;
        };
        let Some(paren_end_rel) = line[paren_start + 1..].find(')') else {
            continue;
        };
        let paren_end = paren_start + 1 + paren_end_rel;
        let location = &line[paren_start + 1..paren_end];
        let Some((line_no, col_no)) = parse_line_col_pair(location) else {
            continue;
        };
        let rest = line[paren_end + 1..]
            .trim_start()
            .trim_start_matches(':')
            .trim_start();
        let Some(message_start) = rest.find(": ") else {
            continue;
        };
        let header = &rest[..message_start];
        if !header.contains("error TS") && !header.contains("warning TS") {
            continue;
        }
        let code = header
            .split_whitespace()
            .find(|part| part.starts_with("TS"))
            .map(str::to_string);
        diagnostics.push(ParsedDiagnostic {
            file: line[..paren_start].trim().to_string(),
            line: line_no,
            column: col_no,
            severity: if header.contains("warning") {
                "WARN"
            } else {
                "ERROR"
            }
            .into(),
            code,
            message: rest[message_start + 2..].trim().to_string(),
            source: "typescript".into(),
        });
    }
    diagnostics
}

pub(super) fn parse_go_diagnostics(output: &str) -> Vec<ParsedDiagnostic> {
    let mut diagnostics = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi_codes(raw);
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("FAIL")
            || trimmed.starts_with("ok ")
            || trimmed.starts_with('?')
        {
            continue;
        }
        let Some((file, line_no, col_no, message)) = parse_go_diagnostic_line(trimmed) else {
            continue;
        };
        diagnostics.push(ParsedDiagnostic {
            file,
            line: line_no,
            column: col_no,
            severity: "ERROR".into(),
            code: None,
            message,
            source: "go".into(),
        });
    }
    diagnostics
}

fn parse_go_diagnostic_line(line: &str) -> Option<(String, usize, usize, String)> {
    let marker = ".go:";
    let marker_index = line.find(marker)?;
    let file_end = marker_index + ".go".len();
    let file = line[..file_end].trim().to_string();
    let mut rest = &line[file_end + 1..];
    let (line_text, after_line) = split_ascii_number_prefix(rest)?;
    let line_no = line_text.parse::<usize>().ok()?;
    rest = after_line.strip_prefix(':')?;
    let (column, message) = if let Some((col_text, after_col)) = split_ascii_number_prefix(rest) {
        if let Some(after_colon) = after_col.strip_prefix(':') {
            (col_text.parse::<usize>().ok()?, after_colon.trim())
        } else {
            (1, rest.trim())
        }
    } else {
        (1, rest.trim())
    };
    if file.is_empty() || message.is_empty() {
        return None;
    }
    Some((file, line_no, column, message.to_string()))
}

fn split_ascii_number_prefix(value: &str) -> Option<(&str, &str)> {
    let len = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .map(|(index, ch)| index + ch.len_utf8())
        .last()?;
    Some(value.split_at(len))
}

pub(super) fn parse_pyright_diagnostics(output: &str) -> Vec<ParsedDiagnostic> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Vec::new();
    };
    value
        .get("generalDiagnostics")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let file = item.get("file").and_then(Value::as_str)?.to_string();
                    let range = item.get("range")?;
                    let start = range.get("start")?;
                    let line = start.get("line").and_then(Value::as_u64).unwrap_or(0) as usize + 1;
                    let column =
                        start.get("character").and_then(Value::as_u64).unwrap_or(0) as usize + 1;
                    let severity = match item
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "warning" => "WARN",
                        "information" | "hint" => "INFO",
                        _ => "ERROR",
                    }
                    .to_string();
                    Some(ParsedDiagnostic {
                        file,
                        line,
                        column,
                        severity,
                        code: item.get("rule").and_then(Value::as_str).map(str::to_string),
                        message: item
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Python diagnostic")
                            .to_string(),
                        source: "pyright".into(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn parse_python_compileall_diagnostics(output: &str) -> Vec<ParsedDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_line: Option<usize> = None;
    for raw in output.lines() {
        let line = strip_ansi_codes(raw);
        if let Some(file) = parse_compileall_error_file(&line) {
            current_file = Some(file);
            current_line = None;
            continue;
        }
        if let Some((file, line_no)) = parse_python_file_line(&line) {
            current_file = Some(file);
            current_line = Some(line_no);
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("File ")
            || trimmed.starts_with("*** Error compiling")
            || trimmed.starts_with('^')
            || (line.chars().next().is_some_and(|ch| ch.is_whitespace())
                && !trimmed.contains("Error:"))
        {
            continue;
        }
        if let Some(file) = current_file.clone() {
            diagnostics.push(ParsedDiagnostic {
                file,
                line: current_line.unwrap_or(1),
                column: 1,
                severity: "ERROR".into(),
                code: Some("compileall".into()),
                message: trimmed.to_string(),
                source: "python compileall".into(),
            });
            current_file = None;
            current_line = None;
        }
    }
    diagnostics
}

fn parse_compileall_error_file(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("*** Error compiling ")?;
    let rest = rest.trim();
    let quote = rest.chars().next().filter(|ch| *ch == '\'' || *ch == '"')?;
    let after_quote = &rest[quote.len_utf8()..];
    let end = after_quote.find(quote)?;
    Some(after_quote[..end].to_string()).filter(|file| !file.is_empty())
}

fn parse_python_file_line(line: &str) -> Option<(String, usize)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("File ")?;
    let after_quote = rest.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    let file = after_quote[..end].to_string();
    let line_marker = after_quote[end + 1..].split_once("line")?.1;
    let digits = line_marker
        .trim_start_matches(|ch: char| ch == ',' || ch.is_whitespace())
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    Some((file, digits.parse().ok()?))
}

fn parse_file_line_col(value: &str) -> Option<(String, usize, usize)> {
    let mut parts = value.rsplitn(3, ':').collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }
    parts.reverse();
    let file = parts[0].trim().to_string();
    let line = parts[1].trim().parse::<usize>().ok()?;
    let col = parts[2].trim().parse::<usize>().ok()?;
    if file.is_empty() {
        return None;
    }
    Some((file, line, col))
}

fn parse_line_col_pair(value: &str) -> Option<(usize, usize)> {
    let (line, col) = value.split_once(',')?;
    Some((line.trim().parse().ok()?, col.trim().parse().ok()?))
}

pub(super) fn diagnostics_to_json(diagnostics: &[ParsedDiagnostic]) -> Value {
    Value::Array(
        diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "file": diagnostic.file,
                    "line": diagnostic.line,
                    "column": diagnostic.column,
                    "severity": diagnostic.severity,
                    "code": diagnostic.code,
                    "message": diagnostic.message,
                    "source": diagnostic.source,
                })
            })
            .collect(),
    )
}

pub(super) fn diagnostic_severity_number(severity: &str) -> u64 {
    match severity.trim().to_ascii_uppercase().as_str() {
        "ERROR" => 1,
        "WARN" | "WARNING" => 2,
        "INFO" | "INFORMATION" => 3,
        "HINT" => 4,
        _ => 1,
    }
}

fn diagnostic_severity_name(severity: u64) -> &'static str {
    match severity {
        1 => "ERROR",
        2 => "WARN",
        3 => "INFO",
        4 => "HINT",
        _ => "ERROR",
    }
}

pub(super) fn parsed_diagnostic_to_lsp(diagnostic: &ParsedDiagnostic) -> Value {
    let start_line = diagnostic.line.saturating_sub(1);
    let start_col = diagnostic.column.saturating_sub(1);
    json!({
        "file": diagnostic.file,
        "range": {
            "start": {
                "line": start_line,
                "character": start_col,
            },
            "end": {
                "line": start_line,
                "character": start_col.saturating_add(1),
            }
        },
        "severity": diagnostic_severity_number(&diagnostic.severity),
        "code": diagnostic.code,
        "source": diagnostic.source,
        "message": diagnostic.message,
    })
}

pub(super) fn diagnostics_to_lsp_json(diagnostics: &[ParsedDiagnostic]) -> Value {
    Value::Array(diagnostics.iter().map(parsed_diagnostic_to_lsp).collect())
}

pub(super) fn format_lsp_diagnostic(diagnostic: &Value) -> Option<String> {
    let severity = diagnostic
        .get("severity")
        .and_then(Value::as_u64)
        .map(diagnostic_severity_name)
        .unwrap_or("ERROR");
    let start = diagnostic.pointer("/range/start")?;
    let line = start.get("line").and_then(Value::as_u64).unwrap_or(0) + 1;
    let column = start.get("character").and_then(Value::as_u64).unwrap_or(0) + 1;
    let message = diagnostic
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let code = diagnostic
        .get("code")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" [{value}]"))
        .unwrap_or_default();
    let source = diagnostic
        .get("source")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" ({value})"))
        .unwrap_or_default();
    Some(format!(
        "{severity} [{line}:{column}] {message}{code}{source}"
    ))
}

pub(super) fn format_lsp_diagnostics_report(diagnostics: &Value) -> String {
    let Some(items) = diagnostics.as_array() else {
        return String::new();
    };
    let mut by_file: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for diagnostic in items {
        if diagnostic
            .get("severity")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            != 1
        {
            continue;
        }
        let file = diagnostic
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        by_file.entry(file).or_default().push(diagnostic);
    }
    let mut blocks = Vec::new();
    for (file, items) in by_file {
        let total = items.len();
        let mut lines = items
            .into_iter()
            .take(LSP_MAX_PER_FILE)
            .filter_map(format_lsp_diagnostic)
            .collect::<Vec<_>>();
        if total > LSP_MAX_PER_FILE {
            lines.push(format!("... and {} more", total - LSP_MAX_PER_FILE));
        }
        if !lines.is_empty() {
            blocks.push(format!(
                "<diagnostics file=\"{}\">\n{}\n</diagnostics>",
                file.replace('"', "&quot;"),
                lines.join("\n")
            ));
        }
    }
    truncate_output(&blocks.join("\n"), LSP_MAX_TOTAL_CHARS)
}

pub(super) fn format_diagnostics_block(diagnostics: &[ParsedDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return String::new();
    }
    let mut by_file: BTreeMap<String, Vec<&ParsedDiagnostic>> = BTreeMap::new();
    for diagnostic in diagnostics {
        by_file
            .entry(diagnostic.file.clone())
            .or_default()
            .push(diagnostic);
    }
    let mut blocks = Vec::new();
    for (file, items) in by_file {
        let mut lines = Vec::new();
        for diagnostic in items.into_iter().take(20) {
            let code = diagnostic
                .code
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(|value| format!(" [{value}]"))
                .unwrap_or_default();
            lines.push(format!(
                "{} [{}:{}] {}{} ({})",
                diagnostic.severity,
                diagnostic.line,
                diagnostic.column,
                diagnostic.message,
                code,
                diagnostic.source
            ));
        }
        blocks.push(format!(
            "<diagnostics file=\"{}\">\n{}\n</diagnostics>",
            file.replace('"', "&quot;"),
            lines.join("\n")
        ));
    }
    truncate_output(&blocks.join("\n"), 4000)
}

fn strip_ansi_codes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn platform_command(name: &str) -> String {
    if cfg!(windows) && name == "npx" {
        "npx.cmd".into()
    } else {
        name.into()
    }
}

async fn run_diagnostic_command(
    root: &Path,
    command: &DiagnosticCommand,
    timeout_seconds: u64,
) -> Result<std::process::Output, String> {
    let mut child_command = Command::new(&command.program);
    child_command.hide_window();
    let child = child_command
        .args(&command.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to start {}: {error}", command.display))?;
    match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => {
            output.map_err(|error| format!("failed to read {}: {error}", command.display))
        }
        Err(_) => Err(format!(
            "timed out after {}s while running {}",
            timeout_seconds, command.display
        )),
    }
}
