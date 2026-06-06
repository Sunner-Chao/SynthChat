#!/bin/bash

# 快速查看 Git 仓库状态

RECENT_COMMITS=5

while [[ $# -gt 0 ]]; do
    case $1 in
        --recent-commits|-n)
            RECENT_COMMITS="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
fi

cd "$PROJECT_ROOT"

branch_output="$(git branch --show-current 2>/dev/null || true)"
remote_output="$(git remote get-url origin 2>/dev/null || true)"
branch="$(echo "$branch_output" | tr -d '[:space:]')"
remote="$(echo "$remote_output" | tr -d '[:space:]')"

echo -e "\033[32mGit 仓库状态\033[0m"
echo -e "\033[33m当前分支: $branch\033[0m"
echo -e "\033[33m远程仓库: $remote\033[0m"
echo ""

echo -e "\033[36m工作区状态\033[0m"
git status --short

echo ""
echo -e "\033[36m最近提交\033[0m"
git log --oneline -n "$RECENT_COMMITS"

cd "$ORIGINAL_DIR"