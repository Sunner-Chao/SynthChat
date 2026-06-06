#!/bin/bash

# 清空远端仓库内容

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
fi

MESSAGE=""
NO_PUSH=false
KEEP_TEMP=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --message|-m)
            MESSAGE="$2"
            shift 2
            ;;
        --no-push)
            NO_PUSH=true
            shift
            ;;
        --keep-temp)
            KEEP_TEMP=true
            shift
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

resolve_commit_message() {
    if [ -n "$MESSAGE" ] && [ -n "$(echo "$MESSAGE" | tr -d '[:space:]')" ]; then
        echo "$MESSAGE" | tr -d '[:space:]'
        return
    fi

    read -p "请输入 commit 信息（必填）: " input_message
    if [ -z "$(echo "$input_message" | tr -d '[:space:]')" ]; then
        echo "commit 信息不能为空。" >&2
        exit 1
    fi
    echo "$input_message" | tr -d '[:space:]'
}

get_tracked_files() {
    git ls-files 2>/dev/null || true
}

get_remote_branches() {
    local remote_url="$1"
    git ls-remote --heads "$remote_url" 2>/dev/null | while read line; do
        if [ -z "$line" ]; then continue; fi
        local ref="$(echo "$line" | awk '{print $2}')"
        if [ -z "$ref" ]; then continue; fi
        if echo "$ref" | grep -qE '^refs/heads/.+'; then
            echo "$ref" | sed 's|^refs/heads/||'
        fi
    done | sort -u
}

resolve_target_branch() {
    local remote_branches=("$@")
    local current_branch="${remote_branches[-1]}"
    unset 'remote_branches[-1]'

    if [ ${#remote_branches[@]} -eq 0 ]; then
        echo "未检测到远端分支。" >&2
        exit 1
    fi

    echo -e "\033[36m[clear-remote-repo] 远端可选分支：\033[0m"
    for i in "${!remote_branches[@]}"; do
        local branch_name="${remote_branches[$i]}"
        local mark=""
        if [ "$branch_name" = "$current_branch" ]; then
            mark=" (当前本地分支)"
        fi
        echo -e "  \033[37m$((i+1)). $branch_name$mark\033[0m"
    done

    while true; do
        read -p "请输入要清空的远端分支编号或名称（直接回车默认 $current_branch）: " input_value
        if [ -z "$(echo "$input_value" | tr -d '[:space:]')" ]; then
            for b in "${remote_branches[@]}"; do
                if [ "$b" = "$current_branch" ]; then
                    echo "$current_branch"
                    return
                fi
            done
            echo -e "\033[33m[clear-remote-repo] 当前本地分支不在远端列表中，请手动输入分支编号或名称。\033[0m"
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

        echo -e "\033[33m[clear-remote-repo] 未匹配到远端分支，请重新输入。\033[0m"
    done
}

cd "$PROJECT_ROOT"

PROJECT_ROOT="$(find_git_repo_root "$PROJECT_ROOT")"
if [ -z "$PROJECT_ROOT" ]; then
    echo "未能定位 Git 仓库根目录。请在仓库内运行该脚本。" >&2
    exit 1
fi

cd "$PROJECT_ROOT"

current_branch="$(git branch --show-current 2>/dev/null | tr -d '[:space:]' || true)"
if [ -z "$current_branch" ]; then
    echo "未检测到当前分支，无法继续。" >&2
    exit 1
fi

remote_url="$(git remote get-url origin 2>/dev/null | tr -d '[:space:]' || true)"
if [ -z "$remote_url" ]; then
    echo "未检测到 origin 远程。" >&2
    exit 1
fi

mapfile -t remote_branches < <(get_remote_branches "$remote_url")
branch="$(resolve_target_branch "${remote_branches[@]}" "$current_branch")"

echo -e "\033[33m[clear-remote-repo] 当前本地分支: $current_branch\033[0m"
echo -e "\033[33m[clear-remote-repo] 目标远端分支: $branch\033[0m"
echo -e "\033[33m[clear-remote-repo] 远端: $remote_url\033[0m"

read -p "确认执行清空仓库内容吗？(y/N): " confirm
if ! [[ "$confirm" =~ ^(y|yes)$ ]]; then
    echo -e "\033[33m[clear-remote-repo] 已取消操作。\033[0m"
    cd "$ORIGINAL_DIR"
    exit 0
fi

commit_message="$(resolve_commit_message)"

temp_repo="$(mktemp -d -t lstwinhr-clear-XXXXXXXXXX)"
echo -e "\033[36m[clear-remote-repo] 创建临时仓库副本...\033[0m"
echo -e "\033[37m[clear-remote-repo] 临时目录: $temp_repo\033[0m"
git clone --depth 1 --branch "$branch" --single-branch "$remote_url" "$temp_repo"
if [ $? -ne 0 ]; then
    echo "git clone 临时仓库失败。" >&2
    exit 1
fi

cd "$temp_repo"

echo -e "\033[36m[clear-remote-repo] 在临时仓库中删除所有已跟踪文件...\033[0m"
tracked_files="$(get_tracked_files)"
if [ -z "$tracked_files" ]; then
    echo -e "\033[33m[clear-remote-repo] 当前远端分支已经没有已跟踪文件，无需再清空。\033[0m"
    cd "$ORIGINAL_DIR"
    if [ -d "$temp_repo" ] && [ "$KEEP_TEMP" != true ]; then
        rm -rf "$temp_repo"
    fi
    exit 0
fi

git rm -r -f -- .
if [ $? -ne 0 ]; then
    echo "git rm 失败。" >&2
    exit 1
fi

echo -e "\033[33m[clear-remote-repo] 提交信息: $commit_message\033[0m"
git commit -m "$commit_message"
if [ $? -ne 0 ]; then
    echo "git commit 失败。" >&2
    exit 1
fi

if [ "$NO_PUSH" = true ]; then
    echo -e "\033[32m[clear-remote-repo] 已在临时仓库完成提交，未推送到远端（NoPush）。\033[0m"
    if [ "$KEEP_TEMP" != true ]; then
        echo -e "\033[33m[clear-remote-repo] 由于使用了 NoPush，已自动保留临时目录供检查。\033[0m"
        KEEP_TEMP=true
    fi
    cd "$ORIGINAL_DIR"
    if [ -d "$temp_repo" ] && [ "$KEEP_TEMP" != true ]; then
        rm -rf "$temp_repo"
    fi
    exit 0
fi

echo -e "\033[36m[clear-remote-repo] 推送到远端...\033[0m"
echo -e "\033[37m[clear-remote-repo] 使用 --force-with-lease 覆盖远端目标分支...\033[0m"
git push --force-with-lease origin "$branch"
if [ $? -ne 0 ]; then
    echo "git push 失败。" >&2
    exit 1
fi

echo -e "\033[32m[clear-remote-repo] 已完成远端当前分支内容清空。\033[0m"

cd "$ORIGINAL_DIR"

if [ -d "$temp_repo" ] && [ "$KEEP_TEMP" != true ]; then
    rm -rf "$temp_repo"
fi