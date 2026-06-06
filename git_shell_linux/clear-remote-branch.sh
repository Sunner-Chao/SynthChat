#!/bin/bash

# 删除远端分支

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
fi

BRANCH_NAME=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --branch-name|--branch)
            BRANCH_NAME="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

find_git_repo_root() {
    local start_path="$1"
    local current="$(cd "$start_path" && pwd)"
    while [ -n "$current" ]; do
        if [ -d "$current/.git" ]; then
            echo "$current"
            return 0
        fi
        local parent="$(dirname "$current")"
        if [ "$parent" = "$current" ]; then
            break
        fi
        current="$parent"
    done
    echo ""
}

get_remote_branches() {
    local remote_name="$1"
    git ls-remote --heads "$remote_name" 2>/dev/null | while read line; do
        if [ -z "$line" ]; then continue; fi
        local ref="$(echo "$line" | awk '{print $2}')"
        if [ -z "$ref" ]; then continue; fi
        if echo "$ref" | grep -qE '^refs/heads/.+'; then
            echo "$ref" | sed 's|^refs/heads/||'
        fi
    done | sort -u
}

get_remote_default_branch() {
    local remote_name="$1"
    local head_line="$(git ls-remote --symref "$remote_name" HEAD 2>/dev/null | head -1 || true)"
    if [ -z "$head_line" ]; then
        echo ""
        return
    fi
    echo "$head_line" | sed -E 's|.*refs/heads/([^\s]+).*|\1|'
}

resolve_target_branch() {
    local remote_branches=("$@")
    local current_branch="${remote_branches[-2]}"
    local default_branch="${remote_branches[-1]}"
    unset 'remote_branches[-1]'
    unset 'remote_branches[-1]'

    if [ -n "$BRANCH_NAME" ]; then
        echo "$(echo "$BRANCH_NAME" | tr -d '[:space:]')"
        return
    fi

    if [ ${#remote_branches[@]} -eq 0 ]; then
        echo "未检测到远端分支。" >&2
        exit 1
    fi

    echo -e "\033[36m[clear-remote-branch] 远端可删除分支：\033[0m"
    for i in "${!remote_branches[@]}"; do
        local branch="${remote_branches[$i]}"
        local marks=()
        if [ "$branch" = "$current_branch" ]; then marks+=("当前本地分支"); fi
        if [ "$branch" = "$default_branch" ]; then marks+=("远端默认分支"); fi
        local suffix=""
        if [ ${#marks[@]} -gt 0 ]; then
            suffix=" ($(IFS=' / '; echo "${marks[*]}"))"
        fi
        echo -e "  \033[37m$((i+1)). $branch$suffix\033[0m"
    done

    while true; do
        read -p "请输入要删除的远端分支编号或名称: " input_value
        if [ -z "$(echo "$input_value" | tr -d '[:space:]')" ]; then
            echo -e "\033[33m[clear-remote-branch] 分支名不能为空，请重新输入。\033[0m"
            continue
        fi

        local normalized="$(echo "$input_value" | tr -d '[:space:]')"

        if [[ "$normalized" =~ ^[0-9]+$ ]]; then
            local index="$normalized"
            if [ "$index" -ge 1 ] && [ "$index" -le ${#remote_branches[@]} ]; then
                echo "${remote_branches[$((index-1))]}"
                return
            fi
        fi

        for b in "${remote_branches[@]}"; do
            if [ "$b" = "$normalized" ]; then
                echo "$normalized"
                return
            fi
        done

        echo -e "\033[33m[clear-remote-branch] 未匹配到远端分支，请重新输入。\033[0m"
    done
}

cd "$PROJECT_ROOT"

PROJECT_ROOT="$(find_git_repo_root "$PROJECT_ROOT")"
if [ -z "$PROJECT_ROOT" ]; then
    echo "未能定位 Git 仓库根目录。请在仓库内运行该脚本。" >&2
    exit 1
fi

cd "$PROJECT_ROOT"

remote_name='origin'
remote_url="$(git remote get-url "$remote_name" 2>/dev/null | tr -d '[:space:]' || true)"
if [ -z "$remote_url" ]; then
    echo "未检测到 origin 远程。" >&2
    exit 1
fi

current_branch="$(git branch --show-current 2>/dev/null | tr -d '[:space:]' || true)"

mapfile -t remote_branches < <(get_remote_branches "$remote_name")
default_branch="$(get_remote_default_branch "$remote_name")"

branch="$(resolve_target_branch "${remote_branches[@]}" "$current_branch" "$default_branch")"

if [ -n "$default_branch" ] && [ "$branch" = "$default_branch" ]; then
    echo -e "\033[33m[clear-remote-branch] 你选择的是远端默认分支：$branch\033[0m"
    echo -e "\033[33m[clear-remote-branch] 如果该仓库仍将它设为默认分支，GitHub 可能拒绝删除。\033[0m"
fi

echo -e "\033[33m[clear-remote-branch] 当前本地分支: $current_branch\033[0m"
echo -e "\033[33m[clear-remote-branch] 目标远端分支: $branch\033[0m"
echo -e "\033[33m[clear-remote-branch] 远端: $remote_url\033[0m"

read -p "请输入要删除的分支名以确认操作（$branch）: " confirm
if [ "$confirm" != "$branch" ]; then
    echo -e "\033[33m[clear-remote-branch] 已取消操作。\033[0m"
    cd "$ORIGINAL_DIR"
    exit 0
fi

echo -e "\033[36m[clear-remote-branch] 删除远端分支...\033[0m"
git push "$remote_name" --delete "$branch"
if [ $? -ne 0 ]; then
    echo "删除远端分支失败。若该分支是默认分支，请先在 GitHub 上切换默认分支后再试。" >&2
    exit 1
fi

echo -e "\033[32m[clear-remote-branch] 已删除远端分支：$branch\033[0m"

cd "$ORIGINAL_DIR"