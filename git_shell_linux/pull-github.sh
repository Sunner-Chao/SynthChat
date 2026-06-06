#!/bin/bash

# 拉取 GitHub 代码

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
WORK_ROOT="$ORIGINAL_DIR"

source "$SCRIPT_DIR/git-script-profile.sh"
PROFILE_DEFAULTS_RESULT=$(get_git_script_profile)
IFS='|' read -r PROFILE_REPO PROFILE_REMOTE_URL PROFILE_PROTOCOL PROFILE_SSH_HOST PROFILE_REMOTE_NAME <<< "$PROFILE_DEFAULTS_RESULT"

REPOSITORY_URL=""
VERSION=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --repository-url|--url)
            REPOSITORY_URL="$2"
            shift 2
            ;;
        --version|-v)
            VERSION="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

resolve_work_root() {
    local path="$WORK_ROOT"
    if [ "$(basename "$path")" = "git_shell" ] || [ "$(basename "$path")" = "git_shell_linux" ]; then
        dirname "$path"
    else
        echo "$path"
    fi
}

resolve_repository_url() {
    local target_root=$(resolve_work_root)
    local repo_root=$(get_repo_root "$target_root")
    if [ -n "$repo_root" ]; then
        local current_origin=$(git -C "$repo_root" remote get-url origin 2>/dev/null || true)
        if [ -n "$current_origin" ]; then
            echo "$current_origin" | tr -d '[:space:]'
            return
        fi
    fi
    
    if [ -n "$REPOSITORY_URL" ]; then
        echo "$REPOSITORY_URL" | tr -d '[:space:]'
        return
    fi
    
    if [ -n "$PROFILE_REMOTE_URL" ]; then
        echo "$PROFILE_REMOTE_URL" | tr -d '[:space:]'
        return
    fi
    
    if [ -n "$PROFILE_REPO" ]; then
        if [ "$PROFILE_PROTOCOL" = "https" ]; then
            echo "https://github.com/${PROFILE_REPO}.git"
        else
            echo "git@${PROFILE_SSH_HOST}:${PROFILE_REPO}.git"
        fi
        return
    fi
    
    read -p "未找到默认仓库配置，请输入 Repository (owner/repo): " repository
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
    
    local resolved_url=""
    if [ "$protocol" = "https" ]; then
        resolved_url="https://github.com/${repository}.git"
    else
        resolved_url="git@${ssh_host}:${repository}.git"
    fi
    
    save_git_script_profile "$repository" "$resolved_url" "$protocol" "$ssh_host" "$PROFILE_REMOTE_NAME"
    echo -e "\033[32m[pull-github] 已保存默认仓库配置，后续可直接复用。\033[0m"
    echo "$resolved_url"
}

resolve_repo_name() {
    local url="$1"
    local name=$(basename "$url")
    if [ -z "$name" ]; then
        echo 'repo'
        return
    fi
    
    if echo "$name" | grep -q '\.git$'; then
        name="${name%.git}"
    fi
    
    echo "$name"
}

resolve_pull_mode() {
    echo -e "\033[36m[pull-github] 请选择本次拉取模式：\033[0m"
    echo -e "  \033[37m1. 全量拉取（覆盖本地，按远端为准）\033[0m"
    echo -e "  \033[37m2. 仅拉更新内容（默认，尽量保留本地改动）\033[0m"
    read -p "请输入 1 或 2（直接回车默认 2）: " choice
    
    if [ "$choice" = "1" ]; then
        PULL_MODE_RESULT='full_override'
    else
        PULL_MODE_RESULT='update_only'
    fi
}

normalize_version_ref() {
    local input_version="$1"
    if [ -z "$input_version" ]; then
        echo ""
        return
    fi
    echo "$input_version" | tr -d '[:space:]'
}

get_remote_version_tags() {
    git ls-remote --tags origin 2>/dev/null | while read line; do
        if [ -z "$line" ]; then continue; fi
        ref=$(echo "$line" | awk '{print $2}')
        if [ -z "$ref" ]; then continue; fi
        if echo "$ref" | grep -qE '^refs/tags/.+'; then
            echo "$ref" | sed "s|^refs/tags/||; s|\^{}||"
        fi
    done | sort -u
}

get_latest_remote_tag() {
    local tags=("$@")
    if [ ${#tags[@]} -eq 0 ]; then
        echo ""
        return
    fi
    
    local latest_tag="${tags[0]}"
    for tag in "${tags[@]}"; do
        if [ "$tag" != "$latest_tag" ]; then
            # 简单比较，使用字符串排序
            if [[ "$tag" > "$latest_tag" ]]; then
                latest_tag="$tag"
            fi
        fi
    done
    
    echo "$latest_tag"
}

resolve_version_choice() {
    local requested=""
    if [ -n "$VERSION" ]; then
        requested=$(normalize_version_ref "$VERSION")
    else
        read -p "是否切换到指定版本号/标签？(y/N): " use_version
        if ! [[ "$use_version" =~ ^(y|yes)$ ]]; then
            VERSION_CHOICE_RESULT=""
            return
        fi
        requested=''
    fi
    
    mapfile -t remote_tags < <(get_remote_version_tags)
    if [ ${#remote_tags[@]} -eq 0 ]; then
        echo -e "\033[33m[pull-github] 当前远端没有可用版本标签，将继续使用默认分支。\033[0m"
        VERSION_CHOICE_RESULT=""
        return
    fi
    
    local latest_tag=$(get_latest_remote_tag "${remote_tags[@]}")
    echo -e "\033[36m[pull-github] 当前远端可用版本标签：\033[0m"
    echo -e "  \033[37m$(IFS=', '; echo "${remote_tags[*]}")\033[0m"
    echo -e "\033[37m[pull-github] 直接回车将默认使用最新版本：$latest_tag\033[0m"
    
    while true; do
        local input_version="$requested"
        if [ -z "$input_version" ]; then
            read -p "请输入要切换的版本号或标签（例如 1.0.0 / v1.0.0）: " input_version
        fi
        
        if [ -z "$(echo "$input_version" | tr -d '[:space:]')" ]; then
            VERSION_CHOICE_RESULT="$latest_tag"
            return
        fi
        
        local normalized=$(normalize_version_ref "$input_version")
        local candidate_refs=("$normalized")
        if ! echo "$normalized" | grep -q '^v'; then
            candidate_refs+=("v$normalized")
        fi
        
        for candidate in "${candidate_refs[@]}"; do
            for t in "${remote_tags[@]}"; do
                if [ "$t" = "$candidate" ]; then
                    VERSION_CHOICE_RESULT="$candidate"
                    return
                fi
            done
        done
        
        echo -e "\033[33m[pull-github] 远端不存在版本标签 $normalized，请从上面的列表中选择。\033[0m"
        requested=''
    done
}

get_repo_root() {
    local path="$1"
    local current="$path"
    while [ -n "$current" ]; do
        if [ -d "$current/.git" ]; then
            echo "$current"
            return 0
        fi
        parent="$(dirname "$current")"
        if [ "$parent" = "$current" ]; then
            break
        fi
        current="$parent"
    done
    echo ""
}

ensure_repository_context() {
    local target_root=$(resolve_work_root)
    local repo_root=$(get_repo_root "$target_root")
    if [ -n "$repo_root" ]; then
        REPO_ROOT="$repo_root"
        cd "$repo_root"
        return
    fi
    
    echo "当前目录还不是独立 Git 仓库：$target_root" >&2
    echo "请先运行 ./init-git-env.sh 初始化当前目录，然后再执行 ./pull-github.sh。" >&2
    exit 1
}

ensure_origin_url() {
    local resolved_repository_url="$1"
    
    local current_origin=$(git remote get-url origin 2>/dev/null || true)
    if [ -z "$current_origin" ]; then
        git remote add origin "$resolved_repository_url"
        if [ $? -ne 0 ]; then
            echo "配置 origin 远程失败。" >&2
            exit 1
        fi
        return
    fi
    
    if [ "$(echo "$current_origin" | tr -d '[:space:]')" != "$(echo "$resolved_repository_url" | tr -d '[:space:]')" ]; then
        git remote set-url origin "$resolved_repository_url"
        if [ $? -ne 0 ]; then
            echo "更新 origin 远程失败。" >&2
            exit 1
        fi
    fi
}

get_status_lines() {
    git status --short 2>/dev/null || true
}

has_unmerged_files() {
    local unmerged=$(git diff --name-only --diff-filter=U 2>/dev/null || true)
    [ -n "$unmerged" ]
}

write_status_summary() {
    local lines="$1"
    if [ -z "$lines" ]; then
        echo -e "\033[32m[pull-github] 当前工作区干净。\033[0m"
        return
    fi
    
    local tracked=$(echo "$lines" | grep -v '^??' | wc -l)
    local untracked=$(echo "$lines" | grep '^??' | wc -l)
    echo -e "\033[33m[pull-github] 当前工作区存在改动：已跟踪改动 ${tracked##*( )} 个，未跟踪文件 ${untracked##*( )} 个。\033[0m"
}

get_remote_default_branch() {
    local head_info=$(git ls-remote --symref origin HEAD 2>/dev/null || true)
    local head_line=$(echo "$head_info" | grep '^ref:' | head -1)
    if [ -z "$head_line" ]; then
        echo ""
        return
    fi
    
    echo "$head_line" | sed -E 's|^ref:\s*refs/heads/(\S+).*|\1|'
}

ensure_local_branch() {
    local branch="$1"
    
    local current_branch_output=$(git branch --show-current 2>/dev/null || true)
    local current_branch=$(echo "$current_branch_output" | tr -d '[:space:]')
    if [ "$current_branch" = "$branch" ]; then
        return
    fi
    
    if git show-ref --verify --quiet "refs/heads/$branch" 2>/dev/null; then
        git checkout "$branch"
    else
        git checkout -b "$branch" --track "origin/$branch"
    fi
    
    if [ $? -ne 0 ]; then
        echo "切换到分支 $branch 失败。" >&2
        exit 1
    fi
}

invoke_full_branch_pull() {
    local branch="$1"
    
    echo -e "\033[36m[pull-github] 全量拉取默认分支: $branch\033[0m"
    git checkout -f -B "$branch" "origin/$branch"
    if [ $? -ne 0 ]; then
        echo "检出远端分支 $branch 失败。" >&2
        exit 1
    fi
    
    git reset --hard "origin/$branch"
    if [ $? -ne 0 ]; then
        echo "git reset --hard 失败。" >&2
        exit 1
    fi
    
    git clean -fd
    if [ $? -ne 0 ]; then
        echo "git clean -fd 失败。" >&2
        exit 1
    fi
}

invoke_update_branch_pull() {
    local branch="$1"
    
    if has_unmerged_files; then
        echo "当前仓库存在未解决冲突文件。请先处理冲突，或改用全量拉取。" >&2
        exit 1
    fi
    
    ensure_local_branch "$branch"
    write_status_summary "$(get_status_lines)"
    echo -e "\033[36m[pull-github] 拉取默认分支最新更新...\033[0m"
    git pull --rebase --autostash origin "$branch"
    if [ $? -ne 0 ]; then
        echo "git pull --rebase 失败。常见原因：本地冲突、远端变更复杂或当前工作区不干净。" >&2
        exit 1
    fi
}

invoke_full_version_pull() {
    local version_ref="$1"
    
    echo -e "\033[36m[pull-github] 全量切换到版本标签: $version_ref\033[0m"
    git reset --hard HEAD >/dev/null 2>&1 || true
    git clean -fd >/dev/null 2>&1 || true
    git checkout -f "$version_ref"
    if [ $? -ne 0 ]; then
        echo "切换到版本标签 $version_ref 失败。" >&2
        exit 1
    fi
}

invoke_update_version_pull() {
    local version_ref="$1"
    
    if has_unmerged_files; then
        echo "当前仓库存在未解决冲突文件。请先处理冲突，或改用全量拉取。" >&2
        exit 1
    fi
    
    local status_lines=$(get_status_lines)
    local tracked_changes=$(echo "$status_lines" | grep -v '^??' | wc -l)
    if [ "${tracked_changes##*( )}" -gt 0 ]; then
        echo "当前工作区存在已跟踪改动，无法安全切换到指定版本 $version_ref。请先提交/清理，或改用全量拉取。" >&2
        exit 1
    fi
    
    write_status_summary "$status_lines"
    echo -e "\033[36m[pull-github] 切换到版本标签: $version_ref\033[0m"
    git checkout "$version_ref"
    if [ $? -ne 0 ]; then
        echo "切换到版本标签 $version_ref 失败。" >&2
        exit 1
    fi
}

pull_submodules() {
    local repo_root="$1"
    local pull_mode="$2"

    if [ ! -f "$repo_root/.gitmodules" ]; then
        return
    fi

    echo -e "\033[36m[pull-github] 检测到子模块，开始同步子模块...\033[0m"

    # 确保子模块已初始化
    git submodule init
    if [ $? -ne 0 ]; then
        echo -e "\033[33m[pull-github] git submodule init 失败，跳过子模块同步。\033[0m" >&2
        return
    fi

    if [ "$pull_mode" = "full_override" ]; then
        # 全量模式：强制更新子模块到远端对应提交
        git submodule update --init --force --recursive
        if [ $? -ne 0 ]; then
            echo -e "\033[33m[pull-github] 子模块全量更新失败，请手动检查。\033[0m" >&2
            return
        fi
        echo -e "\033[32m[pull-github] 子模块已全量同步完成。\033[0m"
    else
        # 更新模式：拉取子模块远端最新内容
        git submodule update --init --recursive --remote
        if [ $? -ne 0 ]; then
            echo -e "\033[33m[pull-github] 子模块更新失败，请手动检查。\033[0m" >&2
            return
        fi
        echo -e "\033[32m[pull-github] 子模块已更新完成。\033[0m"
    fi
}

resolved_repository_url=$(resolve_repository_url)
ensure_repository_context
ensure_origin_url "$resolved_repository_url"

echo -e "\033[36m[pull-github] 获取远端最新信息...\033[0m"
git fetch --tags origin
if [ $? -ne 0 ]; then
    echo "git fetch 失败。请先检查远端仓库地址、SSH 配置或网络。" >&2
    exit 1
fi

pull_mode=""
version_ref=""

resolve_pull_mode
pull_mode="$PULL_MODE_RESULT"

resolve_version_choice
version_ref="$VERSION_CHOICE_RESULT"

if [ -n "$version_ref" ]; then
    if [ "$pull_mode" = "full_override" ]; then
        invoke_full_version_pull "$version_ref"
    else
        invoke_update_version_pull "$version_ref"
    fi
    
    echo -e "\033[32m[pull-github] 已完成版本切换: $version_ref\033[0m"

    cd "$REPO_ROOT"
    pull_submodules "$REPO_ROOT" "$pull_mode"

    cd "$ORIGINAL_DIR"
    exit 0
fi

default_branch=$(get_remote_default_branch)
if [ -z "$default_branch" ]; then
    echo "未能识别远端默认分支。" >&2
    exit 1
fi

if [ "$pull_mode" = "full_override" ]; then
    invoke_full_branch_pull "$default_branch"
else
    invoke_update_branch_pull "$default_branch"
fi

echo -e "\033[32m[pull-github] 已完成拉取。默认分支: $default_branch\033[0m"

cd "$REPO_ROOT"
pull_submodules "$REPO_ROOT" "$pull_mode"

cd "$ORIGINAL_DIR"
