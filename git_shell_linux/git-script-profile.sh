#!/bin/bash

# Git 脚本配置文件（被其他脚本 source）

get_git_config_value() {
    git config "$@" 2>/dev/null || true
}

save_git_script_profile() {
    local repository="$1"
    local remote_url="$2"
    local protocol="$3"
    local ssh_host="$4"
    local remote_name="$5"

    if [ -n "$repository" ]; then
        git config --global lstwinhr.defaultRepository "$repository" >/dev/null 2>&1 || true
    fi
    if [ -n "$remote_url" ]; then
        git config --global lstwinhr.defaultRemoteUrl "$remote_url" >/dev/null 2>&1 || true
    fi
    if [ -n "$protocol" ]; then
        git config --global lstwinhr.defaultProtocol "$protocol" >/dev/null 2>&1 || true
    fi
    if [ -n "$ssh_host" ]; then
        git config --global lstwinhr.defaultSshHost "$ssh_host" >/dev/null 2>&1 || true
    fi
    if [ -n "$remote_name" ]; then
        git config --global lstwinhr.defaultRemoteName "$remote_name" >/dev/null 2>&1 || true
    fi
}

get_git_script_profile() {
    local repository=$(get_git_config_value --global --get lstwinhr.defaultRepository)
    local remote_url=$(get_git_config_value --global --get lstwinhr.defaultRemoteUrl)
    local protocol=$(get_git_config_value --global --get lstwinhr.defaultProtocol)
    local ssh_host=$(get_git_config_value --global --get lstwinhr.defaultSshHost)
    local remote_name=$(get_git_config_value --global --get lstwinhr.defaultRemoteName)

    if [ -z "$protocol" ]; then protocol='ssh'; fi
    if [ -z "$ssh_host" ]; then ssh_host='github.com'; fi
    if [ -z "$remote_name" ]; then remote_name='origin'; fi

    echo "${repository}|${remote_url}|${protocol}|${ssh_host}|${remote_name}"
}
