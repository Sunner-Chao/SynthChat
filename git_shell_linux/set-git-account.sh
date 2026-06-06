#!/bin/bash

# 设置 Git 账号

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
fi

USER_NAME=""
EMAIL=""
GLOBAL_MODE=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --user-name|-u)
            USER_NAME="$2"
            shift 2
            ;;
        --email|-e)
            EMAIL="$2"
            shift 2
            ;;
        --global)
            GLOBAL_MODE=true
            shift
            ;;
        *)
            shift
            ;;
    esac
done

cd "$PROJECT_ROOT"

if [ -z "$USER_NAME" ]; then
    read -p "请输入 Git 用户名: " USER_NAME
fi

if [ -z "$(echo "$USER_NAME" | tr -d '[:space:]')" ]; then
    echo "Git 用户名不能为空。" >&2
    exit 1
fi

if [ -z "$EMAIL" ]; then
    read -p "请输入 Git 邮箱: " EMAIL
fi

if [ -z "$(echo "$EMAIL" | tr -d '[:space:]')" ]; then
    echo "Git 邮箱不能为空。" >&2
    exit 1
fi

if [ "$GLOBAL_MODE" != true ]; then
    read -p "是否写入全局配置？(y/N): " use_global
    if [[ "$use_global" =~ ^(y|yes)$ ]]; then
        GLOBAL_MODE=true
    fi
fi

scope_args=()
if [ "$GLOBAL_MODE" = true ]; then
    scope_args+=("--global")
fi

echo -e "\033[36m[set-git-account] 设置 Git 用户名: $USER_NAME\033[0m"
git config "${scope_args[@]}" user.name "$USER_NAME"
if [ $? -ne 0 ]; then
    echo "设置 git user.name 失败" >&2
    exit 1
fi

echo -e "\033[36m[set-git-account] 设置 Git 邮箱: $EMAIL\033[0m"
git config "${scope_args[@]}" user.email "$EMAIL"
if [ $? -ne 0 ]; then
    echo "设置 git user.email 失败" >&2
    exit 1
fi

echo -e "\033[32m[set-git-account] 当前配置如下：\033[0m"
git config "${scope_args[@]}" --get user.name
git config "${scope_args[@]}" --get user.email

cd "$ORIGINAL_DIR"