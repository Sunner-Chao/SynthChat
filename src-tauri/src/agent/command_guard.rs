use std::env;

use crate::{
    error::{AppError, AppResult},
    models::AgentDefinition,
};

pub(super) fn ensure_command_not_hardline(command: &str) -> AppResult<()> {
    if let Some(reason) = hardline_command_reason(command) {
        Err(AppError::BadRequest(format!(
            "command blocked by hardline safety guard: {reason}"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn hardline_command_reason(command: &str) -> Option<String> {
    let lowered = normalize_command_for_guard(command);
    let tokens = command_guard_tokens(&lowered);

    if contains_sudo_stdin(&tokens) && env::var_os("SUDO_PASSWORD").is_none() {
        return Some("sudo -S without configured SUDO_PASSWORD".into());
    }
    if lowered.contains(":(){:|:&};:") || lowered.contains(":() { : | : & }; :") {
        return Some("fork bomb".into());
    }
    if contains_token_prefix(&tokens, "mkfs") || tokens.iter().any(|token| token == "format-volume")
    {
        return Some("filesystem format command".into());
    }
    if token_starts_command(&tokens, "shutdown")
        || token_starts_command(&tokens, "reboot")
        || token_starts_command(&tokens, "halt")
        || token_starts_command(&tokens, "poweroff")
        || powershell_destructive_system_command(&tokens)
    {
        return Some("system shutdown/reboot".into());
    }
    if contains_kill_all(&tokens) {
        return Some("kill all processes".into());
    }
    if contains_raw_device_write(&lowered, &tokens) {
        return Some("raw block device write".into());
    }
    if contains_recursive_delete_of_protected_root(&tokens) {
        return Some("recursive delete of protected root".into());
    }
    if contains_sensitive_system_write(&lowered, &tokens) {
        return Some("sensitive system or credential path write".into());
    }
    if contains_windows_disk_destruction(&tokens) {
        return Some("disk destruction command".into());
    }
    None
}

pub(super) fn dangerous_command_reason(command: &str) -> Option<String> {
    if let Some(reason) = hardline_command_reason(command) {
        return Some(reason);
    }
    let lowered = normalize_command_for_guard(command);
    let tokens = command_guard_tokens(&lowered);

    if contains_recursive_delete(&tokens) {
        return Some("recursive delete".into());
    }
    if contains_world_writable_chmod(&tokens) {
        return Some("world-writable permissions".into());
    }
    if contains_recursive_chown_root(&tokens) {
        return Some("recursive chown to root".into());
    }
    if contains_destructive_sql(&lowered) {
        return Some("destructive SQL without narrow guard".into());
    }
    if contains_service_lifecycle_change(&tokens) {
        return Some("stop/restart/disable system service".into());
    }
    if contains_force_process_kill(&tokens) {
        return Some("force kill processes".into());
    }
    if contains_shell_or_script_inline_execution(&tokens) {
        return Some("inline shell/script execution".into());
    }
    if contains_remote_script_execution(&lowered) {
        return Some("remote content piped to shell".into());
    }
    if contains_sensitive_project_write(&lowered, &tokens) {
        return Some("overwrite project env/config file".into());
    }
    if contains_find_or_xargs_delete(&tokens) {
        return Some("bulk delete via find/xargs".into());
    }
    if contains_gateway_or_container_lifecycle(&tokens) {
        return Some("gateway/container lifecycle change".into());
    }
    if contains_git_destructive_operation(&tokens) {
        return Some("destructive git operation".into());
    }
    if contains_chmod_execute_then_run(&lowered) {
        return Some("chmod +x followed by immediate execution".into());
    }
    if contains_sudo_privilege_flag(&tokens) {
        return Some("sudo privilege flag".into());
    }
    None
}

fn normalize_command_for_guard(command: &str) -> String {
    command
        .to_lowercase()
        .replace("\r\n", "\n")
        .replace('\\', "/")
        .chars()
        .map(|ch| {
            if matches!(ch, '"' | '\'' | '`') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn command_guard_tokens(command: &str) -> Vec<String> {
    command
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '(' | ')' | '{' | '}')
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn contains_token_prefix(tokens: &[String], prefix: &str) -> bool {
    tokens.iter().any(|token| token.starts_with(prefix))
}

fn token_starts_command(tokens: &[String], command: &str) -> bool {
    tokens.iter().any(|token| token == command)
}

fn contains_sudo_stdin(tokens: &[String]) -> bool {
    tokens
        .windows(2)
        .any(|pair| pair[0] == "sudo" && pair[1].starts_with("-s"))
}

fn contains_kill_all(tokens: &[String]) -> bool {
    tokens
        .windows(2)
        .any(|pair| pair[0] == "kill" && pair[1].trim_start_matches('-') == "1")
}

fn contains_raw_device_write(command: &str, tokens: &[String]) -> bool {
    let raw_device = |value: &str| {
        value.starts_with("/dev/sd")
            || value.starts_with("/dev/nvme")
            || value.starts_with("/dev/hd")
            || value.starts_with("/dev/mmcblk")
            || value.starts_with("/dev/vd")
            || value.starts_with("/dev/xvd")
    };
    if command.contains(">/dev/") || command.contains("> /dev/") {
        return tokens
            .iter()
            .any(|token| raw_device(token.trim_start_matches('>')));
    }
    tokens
        .windows(2)
        .any(|pair| pair[0] == "dd" && pair[1].starts_with("of=/dev/"))
        || tokens.iter().any(|token| {
            token.starts_with("of=/dev/") && raw_device(token.trim_start_matches("of="))
        })
}

fn contains_recursive_delete_of_protected_root(tokens: &[String]) -> bool {
    let delete_command = |token: &str| {
        matches!(
            token,
            "rm" | "remove-item" | "del" | "erase" | "rd" | "rmdir"
        )
    };
    let recursive_flag = |token: &str| {
        token.contains('r') && token.starts_with('-')
            || token == "/s"
            || token == "-recurse"
            || token == "-recursive"
    };
    let protected_target = |token: &str| {
        let cleaned = if token == "/" {
            token
        } else {
            token.trim_end_matches('/')
        };
        matches!(
            cleaned,
            "/" | "/*"
                | "/home"
                | "/root"
                | "/etc"
                | "/usr"
                | "/var"
                | "/bin"
                | "/sbin"
                | "/boot"
                | "/lib"
                | "~"
                | "$home"
                | "${home}"
                | "c:"
                | "c:/"
                | "c:/*"
        )
    };

    for (index, token) in tokens.iter().enumerate() {
        if !delete_command(token) {
            continue;
        }
        let window = &tokens[index + 1..tokens.len().min(index + 8)];
        if window.iter().any(|item| recursive_flag(item))
            && window.iter().any(|item| protected_target(item))
        {
            return true;
        }
    }
    false
}

fn contains_sensitive_system_write(command: &str, tokens: &[String]) -> bool {
    let sensitive_path = command.contains("/etc/sudoers")
        || command.contains("/.ssh/")
        || command.contains("/authorized_keys")
        || command.contains("/.netrc")
        || command.contains("/.pgpass")
        || command.contains("/.npmrc")
        || command.contains("/.pypirc")
        || command.contains("/.hermes/.env")
        || command.contains("config.yaml");
    if !sensitive_path {
        return false;
    }
    let write_verbs = [
        ">",
        ">>",
        "tee",
        "sed",
        "perl",
        "python",
        "python3",
        "node",
        "copy",
        "cp",
        "mv",
        "move",
        "set-content",
        "add-content",
        "out-file",
        "new-item",
        "remove-item",
        "chmod",
        "chown",
        "icacls",
        "visudo",
    ];
    tokens
        .iter()
        .any(|token| write_verbs.contains(&token.as_str()))
        || command.contains('>')
        || command.contains(">>")
}

fn powershell_destructive_system_command(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| matches!(token.as_str(), "stop-computer" | "restart-computer"))
}

fn contains_windows_disk_destruction(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "clear-disk" | "initialize-disk" | "format-volume" | "repair-volume"
        )
    }) || tokens.windows(2).any(|pair| {
        (pair[0] == "diskpart" && pair[1].contains("/s"))
            || (pair[0] == "cipher" && pair[1] == "/w:c:")
    })
}

fn contains_recursive_delete(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "rm"
            && tokens[index + 1..tokens.len().min(index + 6)]
                .iter()
                .any(|item| item == "--recursive" || (item.starts_with('-') && item.contains('r')))
    })
}

fn contains_world_writable_chmod(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "chmod"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| {
                    matches!(item.as_str(), "777" | "666")
                        || item.contains("o+w")
                        || item.contains("a+w")
                        || item.contains("o+rw")
                        || item.contains("a+rw")
                        || item.contains("o+rwx")
                        || item.contains("a+rwx")
                })
    })
}

fn contains_recursive_chown_root(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "chown"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| item == "--recursive" || (item.starts_with('-') && item.contains('r')))
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| item.starts_with("root") || item.ends_with(":root"))
    })
}

fn contains_destructive_sql(command: &str) -> bool {
    command.contains("drop table")
        || command.contains("drop database")
        || command.contains("truncate table")
        || (command.contains("delete from") && !command.contains(" where "))
}

fn contains_service_lifecycle_change(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "systemctl"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| {
                    matches!(
                        item.as_str(),
                        "stop" | "restart" | "disable" | "mask" | "poweroff" | "reboot" | "halt"
                    )
                })
    })
}

fn contains_force_process_kill(tokens: &[String]) -> bool {
    tokens.windows(2).any(|pair| {
        (pair[0] == "pkill" && pair[1].contains('9'))
            || (pair[0] == "killall"
                && (pair[1].contains('9') || pair[1].contains("kill") || pair[1].contains("-r")))
    })
}

fn contains_shell_or_script_inline_execution(tokens: &[String]) -> bool {
    tokens.windows(2).any(|pair| {
        (matches!(pair[0].as_str(), "bash" | "sh" | "zsh" | "ksh")
            && pair[1].starts_with('-')
            && pair[1].contains('c'))
            || (matches!(
                pair[0].as_str(),
                "python" | "python2" | "python3" | "perl" | "ruby" | "node"
            ) && pair[1].starts_with('-')
                && (pair[1].contains('c') || pair[1].contains('e')))
    })
}

fn contains_remote_script_execution(command: &str) -> bool {
    (command.contains("curl ") || command.contains("wget "))
        && (command.contains("| sh")
            || command.contains("| bash")
            || command.contains("|/bin/sh")
            || command.contains("|/bin/bash")
            || command.contains("< <("))
}

fn contains_sensitive_project_write(command: &str, tokens: &[String]) -> bool {
    let project_sensitive = command.contains(".env") || command.contains("config.yaml");
    if !project_sensitive {
        return false;
    }
    command.contains('>')
        || tokens
            .iter()
            .any(|token| matches!(token.as_str(), "tee" | "cp" | "mv" | "install"))
}

fn contains_find_or_xargs_delete(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        (token == "xargs"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| item == "rm"))
            || (token == "find"
                && (tokens[index + 1..].iter().any(|item| item == "-delete")
                    || (tokens[index + 1..]
                        .iter()
                        .any(|item| item == "-exec" || item == "-execdir")
                        && tokens[index + 1..].iter().any(|item| item == "rm"))))
    })
}

fn contains_gateway_or_container_lifecycle(tokens: &[String]) -> bool {
    tokens.windows(3).any(|items| {
        (items[0] == "docker"
            && items[1] == "compose"
            && matches!(items[2].as_str(), "restart" | "stop" | "kill" | "down"))
            || (items[0] == "hermes"
                && items[1] == "gateway"
                && matches!(items[2].as_str(), "stop" | "restart"))
    }) || tokens.windows(2).any(|items| {
        (items[0] == "docker" && matches!(items[1].as_str(), "restart" | "stop" | "kill"))
            || (items[0] == "hermes" && items[1] == "update")
    })
}

fn contains_git_destructive_operation(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "git"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .windows(2)
                .any(|pair| {
                    (pair[0] == "reset" && pair[1] == "--hard")
                        || (pair[0] == "branch" && pair[1] == "-d")
                })
            || (token == "git"
                && tokens[index + 1..tokens.len().min(index + 10)]
                    .iter()
                    .any(|item| item == "--force" || item == "-f")
                && tokens[index + 1..tokens.len().min(index + 4)]
                    .iter()
                    .any(|item| item == "push" || item == "clean"))
    })
}

fn contains_chmod_execute_then_run(command: &str) -> bool {
    command.contains("chmod +x")
        && (command.contains("; ./")
            || command.contains("&& ./")
            || command.contains("| ./")
            || command.contains("\n./"))
}

fn contains_sudo_privilege_flag(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "sudo"
            && tokens[index + 1..tokens.len().min(index + 8)]
                .iter()
                .any(|item| {
                    item == "--stdin"
                        || item == "--askpass"
                        || item == "-s"
                        || item == "-a"
                        || (item.starts_with('-') && (item.contains('s') || item.contains('a')))
                })
    })
}

pub(super) fn ensure_shell_allowed(agent: &AgentDefinition) -> AppResult<()> {
    if agent.allow_shell {
        Ok(())
    } else {
        Err(AppError::BadRequest(shell_disabled_message(agent, None)))
    }
}

pub(super) fn shell_disabled_message(agent: &AgentDefinition, tool_name: Option<&str>) -> String {
    let tool = tool_name
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" for tool '{value}'"))
        .unwrap_or_default();
    format!(
        "当前 Agent 配置禁用了 shell 命令执行{tool}：agent.allowShell=false for agent {} ({}). 这不是系统沙箱禁用；如需使用 terminal/process/execute_code，请在 Agent 配置页打开 allowShell。",
        agent.id, agent.name
    )
}
