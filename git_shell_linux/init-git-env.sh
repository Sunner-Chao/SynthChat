#!/bin/bash

# 初始化 Git 环境

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$SCRIPT_DIR"

source "$SCRIPT_DIR/git-script-profile.sh"

if [ "$(basename "$SCRIPT_DIR")" = "git_shell" ] || [ "$(basename "$SCRIPT_DIR")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$PROJECT_ROOT")"
fi

USER_NAME=""
EMAIL=""
GLOBAL_ACCOUNT=false

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
            GLOBAL_ACCOUNT=true
            shift
            ;;
        *)
            shift
            ;;
    esac
done

write_step() {
    local title="$1"
    echo ""
    echo -e "\033[36m== $title ==\033[0m"
}

write_ok() {
    local message="$1"
    echo -e "  \033[32m$message\033[0m"
}

write_warn_line() {
    local message="$1"
    echo -e "  \033[33m$message\033[0m"
}

write_info_line() {
    local message="$1"
    echo -e "  \033[37m$message\033[0m"
}

resolve_repository_url_interactive() {
    local profile_result=$(get_git_script_profile)
    IFS='|' read -r profile_repository profile_remote_url profile_protocol profile_ssh_host profile_remote_name <<< "$profile_result"
    
    if [ -n "$profile_remote_url" ]; then
        read -p "检测到已保存的默认仓库地址，是否直接使用？(Y/n): " use_saved
        if [ -z "$use_saved" ] || [[ "$use_saved" =~ ^(y|yes)$ ]]; then
            echo "${profile_repository}|${profile_remote_url}|${profile_protocol}|${profile_ssh_host}|${profile_remote_name}"
            return
        fi
    fi
    
    read -p "请输入 Repository (owner/repo): " repository
    if [ -z "$(echo "$repository" | tr -d '[:space:]')" ]; then
        echo "Repository 不能为空。" >&2
        exit 1
    fi
    
    if ! echo "$repository" | grep -qE '^[^/]+/[^/]+$'; then
        echo "Repository 格式必须是 owner/repo，例如 Sunner-Chao/LStwinHR-dev。" >&2
        exit 1
    fi
    
    read -p "请选择协议 ssh/https（直接回车默认 ssh）: " protocol_input
    if [ -z "$(echo "$protocol_input" | tr -d '[:space:]')" ]; then
        protocol='ssh'
    else
        protocol=$(echo "$protocol_input" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')
    fi
    
    if [ "$protocol" != "ssh" ] && [ "$protocol" != "https" ]; then
        echo "协议必须是 ssh 或 https。" >&2
        exit 1
    fi
    
    ssh_host='github.com'
    if [ "$protocol" = "ssh" ]; then
        read -p "是否使用 SSH Host 别名？(y/N): " use_alias
        if [[ "$use_alias" =~ ^(y|yes)$ ]]; then
            read -p "请输入 SSH Host 别名（直接回车默认 github-sunner）: " alias_input
            if [ -z "$(echo "$alias_input" | tr -d '[:space:]')" ]; then
                ssh_host='github-sunner'
            else
                ssh_host=$(echo "$alias_input" | tr -d '[:space:]')
            fi
        fi
    fi
    
    local remote_url=""
    if [ "$protocol" = "https" ]; then
        remote_url="https://github.com/${repository}.git"
    else
        remote_url="git@${ssh_host}:${repository}.git"
    fi
    
    echo "${repository}|${remote_url}|${protocol}|${ssh_host}|origin"
}

resolve_initial_branch_name() {
    read -p "请选择初始化默认分支名（直接回车默认 main）: " branch_input
    if [ -z "$(echo "$branch_input" | tr -d '[:space:]')" ]; then
        echo 'main'
        return
    fi
    
    branch_name=$(echo "$branch_input" | tr -d '[:space:]')
    if echo "$branch_name" | grep -q '[[:space:]]'; then
        echo "分支名不能包含空白字符。" >&2
        exit 1
    fi
    
    echo "$branch_name"
}

cd "$PROJECT_ROOT"

write_step "检查 Git"
if ! command -v git &> /dev/null; then
    echo "未检测到 Git。请先安装 Git 后重试。" >&2
    exit 1
fi
write_ok "Git 已安装: $(git --version | tr -d '[:space:]')"

write_step "检查当前目录"
write_ok "当前项目目录: $PROJECT_ROOT"
is_repo=false
if [ -d "$PROJECT_ROOT/.git" ]; then
    is_repo=true
    write_ok "当前目录已经是 Git 仓库"
    
    repo_root=$(git rev-parse --show-toplevel 2>/dev/null || true)
    real_git_dir=$(git rev-parse --git-dir 2>/dev/null || true)
    branch=$(git branch --show-current 2>/dev/null || true)
    upstream=$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true)
    origin_url=$(git remote get-url origin 2>/dev/null || true)
    last_commit=$(git log -1 --pretty=format:'%h %s' 2>/dev/null || true)
    
    status_lines=$(git status --short 2>/dev/null || true)
    tracked_count=$(echo "$status_lines" | grep -v '^??' | wc -l)
    untracked_count=$(echo "$status_lines" | grep '^??' | wc -l)
    
    if [ -n "$repo_root" ]; then write_info_line "仓库根目录: $repo_root"; fi
    if [ -n "$real_git_dir" ]; then write_info_line ".git 目录: $real_git_dir"; fi
    if [ -n "$branch" ]; then
        write_info_line "当前分支: $branch"
    else
        write_warn_line "当前未处于正常分支，可能是 detached HEAD。"
    fi
    if [ -n "$upstream" ]; then
        write_info_line "上游分支: $upstream"
    else
        write_warn_line "当前分支尚未配置上游分支。"
    fi
    if [ -n "$origin_url" ]; then
        write_info_line "origin: $origin_url"
    else
        write_warn_line "当前仓库尚未配置 origin。"
    fi
    if [ -n "$last_commit" ]; then
        write_info_line "最近提交: $last_commit"
    fi
    write_info_line "工作区状态: 已跟踪改动 ${tracked_count##*( )} 个，未跟踪文件 ${untracked_count##*( )} 个"
else
    write_warn_line "当前目录还不是 Git 仓库"
fi

if [ -n "$USER_NAME" ] || [ -n "$EMAIL" ]; then
    write_step "设置 Git 账号"
    args=()
    if [ -n "$USER_NAME" ]; then args+=("--user-name" "$USER_NAME"); fi
    if [ -n "$EMAIL" ]; then args+=("--email" "$EMAIL"); fi
    if [ "$GLOBAL_ACCOUNT" = true ]; then args+=("--global"); fi
    "$SCRIPT_DIR/set-git-account.sh" "${args[@]}"
else
    write_step "检查 Git 账号"
    local_name_output=$(git config --get user.name 2>/dev/null || true)
    local_email_output=$(git config --get user.email 2>/dev/null || true)
    global_name_output=$(git config --global --get user.name 2>/dev/null || true)
    global_email_output=$(git config --global --get user.email 2>/dev/null || true)
    local_name=$(echo "$local_name_output" | tr -d '[:space:]')
    local_email=$(echo "$local_email_output" | tr -d '[:space:]')
    global_name=$(echo "$global_name_output" | tr -d '[:space:]')
    global_email=$(echo "$global_email_output" | tr -d '[:space:]')
    
    if [ -n "$local_name" ] && [ -n "$local_email" ]; then
        write_ok "本地账号: $local_name <$local_email>"
    elif [ -n "$global_name" ] && [ -n "$global_email" ]; then
        write_ok "全局账号: $global_name <$global_email>"
    else
        write_warn_line "未检测到 Git 用户名/邮箱。"
        write_warn_line '可执行: ./set-git-account.sh'
    fi
fi

write_step "检查 GitHub CLI"
if command -v gh &> /dev/null; then
    write_ok "gh 已安装: $(gh --version | head -1 | tr -d '[:space:]')"
    if gh auth status &>/dev/null; then
        write_ok "gh 已登录"
    else
        write_warn_line "gh 已安装，但当前未登录。可执行: gh auth login"
    fi
else
    write_warn_line "未检测到 gh。可根据你的发行版安装 GitHub CLI"
fi

write_step "初始化仓库"
if [ "$is_repo" != true ]; then
    read -p "是否将当前目录初始化为独立 Git 仓库？(Y/n): " init_choice
    if [ -z "$init_choice" ] || [[ "$init_choice" =~ ^(y|yes)$ ]]; then
        initial_branch=$(resolve_initial_branch_name)
        
        git init -b "$initial_branch" >/dev/null 2>&1 || {
            git init >/dev/null 2>&1 || {
                echo "git init 失败。" >&2
                exit 1
            }
            
            git branch -M "$initial_branch" >/dev/null 2>&1 || {
                echo "已初始化仓库，但设置默认分支 $initial_branch 失败。" >&2
                exit 1
            }
        }
        
        write_ok "已初始化当前目录为 Git 仓库"
        write_ok "初始化分支: $initial_branch"
        is_repo=true
    else
        write_warn_line "已跳过仓库初始化。"
    fi
else
    write_ok "无需重复初始化"
fi

write_step "配置远程仓库"
if [ "$is_repo" = true ]; then
    existing_remotes=$(git remote 2>/dev/null || true)
    origin_url=''
    if echo "$existing_remotes" | grep -q '^origin$'; then
        origin_url_output=$(git remote get-url origin 2>/dev/null || true)
        origin_url=$(echo "$origin_url_output" | tr -d '[:space:]')
    fi
    if [ -n "$origin_url" ]; then
        write_ok "origin: $origin_url"
        read -p "是否重新配置 origin？(y/N): " reset_remote
        if [[ "$reset_remote" =~ ^(y|yes)$ ]]; then
            remote_info=$(resolve_repository_url_interactive)
            IFS='|' read -r repo_info_repo repo_info_url repo_info_proto repo_info_ssh repo_info_rname <<< "$remote_info"
            git remote set-url origin "$repo_info_url"
            if [ $? -ne 0 ]; then
                echo "更新 origin 远程失败。" >&2
                exit 1
            fi
            save_git_script_profile "$repo_info_repo" "$repo_info_url" "$repo_info_proto" "$repo_info_ssh" "$repo_info_rname"
            write_ok "已更新 origin: $repo_info_url"
        fi
    else
        read -p "当前仓库尚未配置 origin，是否现在配置？(Y/n): " set_remote
        if [ -z "$set_remote" ] || [[ "$set_remote" =~ ^(y|yes)$ ]]; then
            remote_info=$(resolve_repository_url_interactive)
            IFS='|' read -r repo_info_repo repo_info_url repo_info_proto repo_info_ssh repo_info_rname <<< "$remote_info"
            git remote add origin "$repo_info_url"
            if [ $? -ne 0 ]; then
                echo "新增 origin 远程失败。" >&2
                exit 1
            fi
            save_git_script_profile "$repo_info_repo" "$repo_info_url" "$repo_info_proto" "$repo_info_ssh" "$repo_info_rname"
            write_ok "已新增 origin: $repo_info_url"
        else
            write_warn_line "已跳过 origin 配置。"
        fi
    fi
fi

write_step "检查远程同步能力"
origin_url_output=$(git remote get-url origin 2>/dev/null || true)
origin_url=$(echo "$origin_url_output" | tr -d '[:space:]')
if [ -n "$origin_url" ]; then
    if git ls-remote origin >/dev/null 2>&1; then
        write_ok "已验证可访问远程仓库"
    else
        write_warn_line "无法访问远程仓库，请检查 SSH / HTTPS 认证"
    fi
else
    write_warn_line "当前没有 origin，暂时无法验证远程访问能力。"
fi

write_step "常用下一步"
echo -e '  \033[37m./git-quick-status.sh\033[0m'
echo -e '  \033[37m./pull-github.sh\033[0m'
echo -e '  \033[37m./push-github.sh\033[0m'

cd "$ORIGINAL_DIR"
