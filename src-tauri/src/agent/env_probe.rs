use std::{env, path::Path, process::Command};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::AgentDefinition,
    process_utils::CommandWindowExt,
};

use super::workspace_root;

const DEFAULT_COMMANDS: &[&str] = &[
    "git", "rg", "fd", "python", "python3", "pip", "uv", "node", "npm", "pnpm", "cargo", "rustc",
    "go", "docker",
];

pub(super) fn env_probe_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let root = workspace_root(agent)?;
    let mut commands = payload
        .get("commands")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .take(40)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            DEFAULT_COMMANDS
                .iter()
                .map(|value| value.to_string())
                .collect()
        });
    commands.sort();
    commands.dedup();
    let command_status = commands
        .iter()
        .map(|command| {
            let path = find_command_on_path(command);
            json!({
                "name": command,
                "available": path.is_some(),
                "path": path
            })
        })
        .collect::<Vec<_>>();
    let workspace_signals = workspace_signals(&root);
    let probe = json!({
        "os": {
            "family": env::consts::FAMILY,
            "os": env::consts::OS,
            "arch": env::consts::ARCH
        },
        "terminal": {
            "envType": terminal_env_type(),
            "shell": env::var("SHELL").ok().or_else(|| env::var("ComSpec").ok()),
            "term": env::var("TERM").ok()
        },
        "workspace": {
            "root": root.to_string_lossy(),
            "signals": workspace_signals
        },
        "python": python_probe(),
        "commands": command_status
    });
    Ok(serde_json::to_string_pretty(&probe)?)
}

fn terminal_env_type() -> String {
    env::var("TERMINAL_ENV")
        .unwrap_or_else(|_| "local".into())
        .trim()
        .to_ascii_lowercase()
}

fn workspace_signals(root: &Path) -> Value {
    let candidates = [
        ("git", ".git"),
        ("rust", "Cargo.toml"),
        ("node", "package.json"),
        ("python", "pyproject.toml"),
        ("python_requirements", "requirements.txt"),
        ("go", "go.mod"),
        ("tauri", "src-tauri/tauri.conf.json"),
        ("docker", "Dockerfile"),
        ("docker_compose", "docker-compose.yml"),
    ];
    let mut found = serde_json::Map::new();
    for (name, relative) in candidates {
        found.insert(name.into(), json!(root.join(relative).exists()));
    }
    Value::Object(found)
}

fn python_probe() -> Value {
    let python = python_binary();
    let Some(binary) = python else {
        return json!({
            "available": false,
            "binary": Value::Null,
            "version": Value::Null,
            "pipModule": false,
            "pipOnPath": find_command_on_path("pip").is_some(),
            "uv": find_command_on_path("uv").is_some()
        });
    };
    json!({
        "available": true,
        "binary": binary,
        "version": command_stdout(&binary, &["-c", "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}')"]).ok(),
        "pipModule": command_stdout(&binary, &["-m", "pip", "--version"]).is_ok(),
        "pipOnPath": find_command_on_path("pip").is_some(),
        "pipVersion": command_stdout("pip", &["--version"]).ok(),
        "uv": find_command_on_path("uv").is_some(),
        "pep668ExternallyManaged": python_pep668(&binary)
    })
}

fn python_binary() -> Option<String> {
    for candidate in ["python3", "python"] {
        if find_command_on_path(candidate).is_some() {
            return Some(candidate.into());
        }
    }
    None
}

fn python_pep668(binary: &str) -> bool {
    command_stdout(
        binary,
        &[
            "-c",
            "import os; marker=os.path.join(os.path.dirname(os.__file__), 'EXTERNALLY-MANAGED'); print('yes' if os.path.exists(marker) else 'no')",
        ],
    )
    .map(|value| value.trim() == "yes")
    .unwrap_or(false)
}

fn command_stdout(command: &str, args: &[&str]) -> Result<String, AppError> {
    let output = Command::new(command)
        .hide_window()
        .args(args)
        .output()
        .map_err(AppError::Io)?;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "{command} exited with status {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn find_command_on_path(command: &str) -> Option<String> {
    let path_var = env::var_os("PATH")?;
    let extensions = path_extensions();
    for dir in env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = dir.join(format!("{command}{extension}"));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn path_extensions() -> Vec<String> {
    if cfg!(windows) {
        env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    }
}
