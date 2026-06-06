#!/bin/bash

# Git 配置概览

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
fi

SHOW_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --show-only)
            SHOW_ONLY=true
            shift
            ;;
        *)
            shift
            ;;
    esac
done

cd "$PROJECT_ROOT"

current_branch="$(git branch --show-current 2>/dev/null | tr -d '[:space:]' || true)"
origin_url="$(git remote get-url origin 2>/dev/null | tr -d '[:space:]' || true)"
local_name="$(git config --get user.name 2>/dev/null | tr -d '[:space:]' || true)"
local_email="$(git config --get user.email 2>/dev/null | tr -d '[:space:]' || true)"
global_name="$(git config --global --get user.name 2>/dev/null | tr -d '[:space:]' || true)"
global_email="$(git config --global --get user.email 2>/dev/null | tr -d '[:space:]' || true)"

echo -e "\033[32mGit 配置概览\033[0m"
echo ""
echo -e "\033[33m当前分支: $current_branch\033[0m"
echo -e "\033[33morigin:   $origin_url\033[0m"
echo ""
echo -e "\033[36m本地仓库账号:\033[0m"
echo "  user.name  = $local_name"
echo "  user.email = $local_email"
echo ""
echo -e "\033[36m全局 Git 账号:\033[0m"
echo "  user.name  = $global_name"
echo "  user.email = $global_email"

if [ "$SHOW_ONLY" != true ]; then
    echo ""
    echo -e "\033[37m常用命令示例:\033[0m"
    echo -e "\033[37m  ./set-git-account.sh --user-name \"Your Name\" --email \"you@example.com\"\033[0m"
    echo -e "\033[37m  ./set-git-account.sh --user-name \"Your Name\" --email \"you@example.com\" --global\033[0m"
    echo -e "\033[37m  ./set-git-remote.sh --repository \"owner/repo\" --protocol ssh\033[0m"
    echo -e "\033[37m  ./set-git-remote.sh --repository \"owner/repo\" --protocol https\033[0m"
fi

cd "$ORIGINAL_DIR"