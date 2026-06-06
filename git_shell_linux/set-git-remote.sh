#!/bin/bash

# 设置 Git 远程仓库

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

REPOSITORY=""
REMOTE_NAME='origin'
PROTOCOL='ssh'
SSH_HOST='github.com'

while [[ $# -gt 0 ]]; do
    case $1 in
        --repository|-r)
            REPOSITORY="$2"
            shift 2
            ;;
        --remote-name)
            REMOTE_NAME="$2"
            shift 2
            ;;
        --protocol)
            PROTOCOL="$2"
            shift 2
            ;;
        --ssh-host)
            SSH_HOST="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

if [ -z "$REPOSITORY" ]; then
    read -p "Repository (owner/repo): " REPOSITORY
fi

if [ -z "$(echo "$REPOSITORY" | tr -d '[:space:]')" ]; then
    echo "Repository 不能为空。" >&2
    exit 1
fi

if [ "$PROTOCOL" = "ssh" ] && [ -z "$SSH_HOST_OVERRIDE" ]; then
    read -p "是否使用 SSH Host 别名？(y/N): " use_alias
    if [[ "$use_alias" =~ ^(y|yes)$ ]]; then
        read -p "请输入 SSH Host 别名（直接回车默认 github-sunner）: " alias_input
        if [ -z "$(echo "$alias_input" | tr -d '[:space:]')" ]; then
            SSH_HOST='github-sunner'
        else
            SSH_HOST=$(echo "$alias_input" | tr -d '[:space:]')
        fi
    fi
fi

if ! echo "$REPOSITORY" | grep -qE '^[^/]+/[^/]+$'; then
    echo "Repository 格式必须是 owner/repo，例如 TheRealPiper/LStwinHR" >&2
    exit 1
fi

if [ "$PROTOCOL" = "ssh" ]; then
    remote_url="git@${SSH_HOST}:${REPOSITORY}.git"
else
    remote_url="https://github.com/${REPOSITORY}.git"
fi

has_remote=false
if git remote 2>/dev/null | grep -q "^${REMOTE_NAME}$"; then
    has_remote=true
fi

if [ "$has_remote" = true ]; then
    echo -e "\033[36m[set-git-remote] 更新远程 $REMOTE_NAME -> $remote_url\033[0m"
    git remote set-url "$REMOTE_NAME" "$remote_url"
    if [ $? -ne 0 ]; then
        echo "git remote set-url 失败" >&2
        exit 1
    fi
else
    echo -e "\033[36m[set-git-remote] 新增远程 $REMOTE_NAME -> $remote_url\033[0m"
    git remote add "$REMOTE_NAME" "$remote_url"
    if [ $? -ne 0 ]; then
        echo "git remote add 失败" >&2
        exit 1
    fi
fi

echo -e "\033[32m[set-git-remote] 当前远程配置：\033[0m"
git remote -v
