#!/bin/bash

# 推送到 GitHub

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIGINAL_DIR="$(pwd)"
PROJECT_ROOT="$ORIGINAL_DIR"

if [ "$(basename "$PROJECT_ROOT")" = "git_shell" ] || [ "$(basename "$PROJECT_ROOT")" = "git_shell_linux" ]; then
    PROJECT_ROOT="$(dirname "$PROJECT_ROOT")"
fi

MESSAGE=""
VERSION=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --message|-m)
            MESSAGE="$2"
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

cd "$PROJECT_ROOT"

source "$SCRIPT_DIR/git-script-profile.sh"
PROFILE_DEFAULTS_RESULT=$(get_git_script_profile)

get_current_branch() {
    local branch_output=$(git branch --show-current 2>/dev/null || true)
    if [ -z "$branch_output" ]; then
        echo ""
        return
    fi
    echo "$branch_output" | tr -d '[:space:]'
}

get_status_lines() {
    git status --short 2>/dev/null || true
}

write_status_summary() {
    local lines="$1"
    if [ -z "$lines" ]; then
        echo -e "\033[32m[push-github] 当前工作区干净。\033[0m"
        return
    fi
    
    local tracked=$(echo "$lines" | grep -v '^??' | wc -l)
    local untracked=$(echo "$lines" | grep '^??' | wc -l)
    echo -e "\033[33m[push-github] 检测到改动：已跟踪文件 ${tracked##*( )} 个，未跟踪文件 ${untracked##*( )} 个。\033[0m"
}

normalize_version_tag() {
    local input_version="$1"
    if [ -z "$input_version" ]; then
        echo ""
        return
    fi
    
    local normalized=$(ech/home/sunner/demo_vscode/LS-ZGTo "$input_version" | tr -d '[:space:]')
    if [ -z "$normalized" ]; then
        echo ""
        return
    fi
    
    if ! echo "$normalized" | grep -q '^v'; then
        normalized="v$normalized"
    fi
    
    echo "$normalized"
}

get_local_version_tags() {
    git tag --list 'v*' 2>/dev/null | sort || true
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

resolve_commit_message() {
    if [ -n "$MESSAGE" ] && [ -n "$(echo "$MESSAGE" | tr -d '[:space:]')" ]; then
        echo "$MESSAGE" | tr -d '[:space:]'
        return
    fi
    
    read -p "请输入本次 commit 信息（必填）: " input_message
    if [ -z "$(echo "$input_message" | tr -d '[:space:]')" ]; then
        echo "commit 信息不能为空。" >&2
        exit 1
    fi
    echo "$input_message" | tr -d '[:space:]'
}

resolve_push_mode() {
    echo -e "\033[36m[push-github] 请选择本次推送模式：\033[0m"
    echo -e "  \033[37m1. 全量推（覆盖远端，按本地为准）\033[0m"
    echo -e "  \033[37m2. 仅推更新内容（默认，尽量保留远端现状）\033[0m"
    read -p "请输入 1 或 2（直接回车默认 2）: " choice
    
    if [ "$choice" = "1" ]; then
        PUSH_MODE_RESULT='full_override'
    else
        PUSH_MODE_RESULT='update_only'
    fi
}

resolve_release_mode() {
    echo -e "\033[36m[push-github] 请选择本次发布方式：\033[0m"
    echo -e "  \033[37m1. 默认分支推送（不创建版本 tag）\033[0m"
    echo -e "  \033[37m2. 版本发布推送（创建并同步版本 tag）\033[0m"
    read -p "请输入 1 或 2（直接回车默认 1）: " choice
    
    if [ "$choice" = "2" ]; then
        RELEASE_MODE_RESULT='tag_release'
    else
        RELEASE_MODE_RESULT='branch_only'
    fi
}

resolve_version_tag() {
    local use_tag_release="$1"
    
    if [ "$use_tag_release" != true ]; then
        VERSION_TAG_RESULT=""
        return
    fi
    
    local direct_tag=""
    if [ -n "$VERSION" ]; then
        direct_tag=$(normalize_version_tag "$VERSION")
        if [ -z "$direct_tag" ]; then
            VERSION_TAG_RESULT=""
            return
        fi
    else
        mapfile -t local_tags < <(get_local_version_tags)
        mapfile -t remote_tags < <(get_remote_version_tags)
        
        if [ ${#local_tags[@]} -gt 0 ]; then
            echo -e "\033[37m[push-github] 本地版本标签：$(IFS=', '; echo "${local_tags[*]}")\033[0m"
        else
            echo -e "\033[37m[push-github] 当前本地仓库还没有版本标签。\033[0m"
        fi
        
        if [ ${#remote_tags[@]} -gt 0 ]; then
            echo -e "\033[37m[push-github] 远端版本标签：$(IFS=', '; echo "${remote_tags[*]}")\033[0m"
        else
            echo -e "\033[37m[push-github] 当前远端还没有版本标签。\033[0m"
        fi
        direct_tag=''
    fi
    
    while true; do
        if [ -z "$direct_tag" ]; then
            read -p "请输入新版本号（例如 1.0.0 或 v1.0.0）: " input_version
            direct_tag=$(normalize_version_tag "$input_version")
        fi
        
        if [ -z "$direct_tag" ]; then
            echo -e "\033[33m[push-github] 未输入有效版本号，请重新输入。\033[0m"
            continue
        fi
        
        if git rev-parse "refs/tags/$direct_tag" >/dev/null 2>&1; then
            echo -e "\033[33m[push-github] 版本标签 $direct_tag 已存在，将按当前内容覆盖该标签。\033[0m"
        fi
        
        VERSION_TAG_RESULT="$direct_tag"
        return
    done
}

ensure_version_tag() {
    local version_tag="$1"
    
    if [ -z "$version_tag" ]; then
        return
    fi
    
    if git rev-parse "refs/tags/$version_tag" >/dev/null 2>&1; then
        echo -e "\033[33m[push-github] 本地已存在标签 $version_tag，正在删除旧标签以便重建...\033[0m"
        git tag -d "$version_tag" >/dev/null 2>&1
        if [ $? -ne 0 ]; then
            echo "删除本地旧标签 $version_tag 失败。" >&2
            exit 1
        fi
    fi
    
    echo -e "\033[36m[push-github] 创建版本标签: $version_tag\033[0m"
    git tag -a "$version_tag" -m "release: $version_tag"
    if [ $? -ne 0 ]; then
        echo "git tag 创建失败。" >&2
        exit 1
    fi
}

invoke_push() {
    local branch="$1"
    local force="$2"
    
    if [ "$force" = true ]; then
        echo -e "\033[33m[push-github] 使用 --force-with-lease 推送当前分支...\033[0m"
        git push --force-with-lease -u origin "$branch"
    else
        git push -u origin "$branch"
    fi
}

branch=$(get_current_branch)
if [ -z "$branch" ]; then
    echo "未检测到当前分支，当前可能处于 detached HEAD 状态。请先执行 git switch <branch> 切回分支后再推送。" >&2
    exit 1
fi

release_mode=""
push_mode=""
version_tag=""

resolve_release_mode
release_mode="$RELEASE_MODE_RESULT"
use_tag_release=false
if [ "$release_mode" = "tag_release" ]; then
    use_tag_release=true
fi
resolve_push_mode
push_mode="$PUSH_MODE_RESULT"
force_push=false
if [ "$push_mode" = "full_override" ]; then
    force_push=true
fi

echo -e "\033[33m[push-github] 当前分支: $branch\033[0m"
echo -e "\033[36m[push-github] 获取远端当前分支信息...\033[0m"
git ls-remote --exit-code --heads origin "$branch" >/dev/null 2>&1
remote_branch_exists=$?
if [ $remote_branch_exists -eq 0 ]; then
    git fetch origin "$branch"
    if [ $? -ne 0 ]; then
        echo "git fetch 当前分支失败。请检查远端仓库地址、SSH/网络，或确认远端分支状态是否异常。" >&2
        exit 1
    fi
fi

if [ $remote_branch_exists -eq 0 ]; then
    if git rev-parse HEAD >/dev/null 2>&1; then
        local_has_commits=true
    else
        local_has_commits=false
    fi
    
    if [ "$local_has_commits" = true ]; then
        ahead_behind_output=$(git rev-list --left-right --count "$branch...origin/$branch" 2>/dev/null || true)
        local_ahead=0
        remote_ahead=0
        if [ -n "$ahead_behind_output" ] && [ $? -eq 0 ]; then
            parts=($ahead_behind_output)
            if [ ${#parts[@]} -ge 2 ]; then
                local_ahead=${parts[0]}
                remote_ahead=${parts[1]}
            fi
        fi
    else
        local_ahead=0
        remote_ahead=0
    fi
    
    if [ "$remote_ahead" -gt 0 ] && [ "$force_push" != true ]; then
        echo -e "\033[33m[push-github] 远端比本地领先 $remote_ahead 个提交。\033[0m"
        echo -e "\033[33m[push-github] 当前为仅推更新内容模式，不会自动覆盖远端。\033[0m"
        echo -e "\033[36m[push-github] 可选操作：\033[0m"
        echo -e "  \033[37m1. 取消推送，稍后先 pull\033[0m"
        echo -e "  \033[37m2. 继续普通 push（大概率会被拒绝）\033[0m"
        echo -e "  \033[37m3. 改为全量推（覆盖远端）\033[0m"
        read -p "请输入 1 / 2 / 3（默认 1）: " choice
        if [ -z "$choice" ] || [ "$choice" = "1" ]; then
            echo -e "\033[33m[push-github] 已取消推送。\033[0m"
            cd "$ORIGINAL_DIR"
            exit 0
        fi
        if [ "$choice" = "3" ]; then
            push_mode='full_override'
            force_push=true
        fi
    fi
else
    echo -e "\033[33m[push-github] 远端不存在分支 $branch，后续将创建远端分支。\033[0m"
fi

echo -e "\033[37m[push-github] 当前推送模式: $(if [ "$push_mode" = "full_override" ]; then echo '全量推（覆盖远端）'; else echo '仅推更新内容'; fi)\033[0m"
status_lines=$(get_status_lines)
write_status_summary "$status_lines"

version_tag=""
if [ -n "$(echo "$status_lines" | tr -d '[:space:]')" ]; then
    echo -e "\033[36m[push-github] 暂存当前仓库的所有本地改动...\033[0m"
    git add -A
    if [ $? -ne 0 ]; then
        echo "git add -A 失败。" >&2
        exit 1
    fi
    
    staged_status=$(git diff --cached --name-only 2>/dev/null || true)
    staged_count=$(echo "$staged_status" | grep -c '.' 2>/dev/null || echo "0")
    if [ "$staged_count" -gt 0 ]; then
        MESSAGE=$(resolve_commit_message)
        resolve_version_tag $use_tag_release
        version_tag="$VERSION_TAG_RESULT"
        if [ -n "$version_tag" ]; then
            MESSAGE="$MESSAGE [$version_tag]"
        fi
        
        echo -e "\033[33m[push-github] 提交信息: $MESSAGE\033[0m"
        git commit -m "$MESSAGE"
        if [ $? -ne 0 ]; then
            echo "git commit 失败。" >&2
            exit 1
        fi
        
        ensure_version_tag "$version_tag"
    else
        version_tag=""
        echo -e "\033[33m[push-github] 当前没有可提交的已暂存改动。\033[0m"
    fi
else
    resolve_version_tag $use_tag_release
    version_tag="$VERSION_TAG_RESULT"
    echo -e "\033[33m[push-github] 当前没有本地改动，将直接执行推送。\033[0m"
    if [ -n "$version_tag" ]; then
        ensure_version_tag "$version_tag"
    fi
fi

echo -e "\033[36m[push-github] 推送到 GitHub...\033[0m"
invoke_push "$branch" "$force_push"
if [ $? -ne 0 ]; then
    if [ "$push_mode" = "full_override" ]; then
        echo "git push 失败。当前已按全量推模式执行。常见原因：远端分支受保护、权限不足、SSH 配置错误。" >&2
    else
        echo "git push 失败。常见原因：远端领先、权限不足、SSH 配置错误，或当前仍需要先 pull。" >&2
    fi
    exit 1
fi

if [ -n "$version_tag" ]; then
    echo -e "\033[36m[push-github] 推送版本标签: $version_tag\033[0m"
    echo -e "\033[37m[push-github] 若远端已存在同名标签，将按当前本地版本覆盖...\033[0m"
    git push --force origin "refs/tags/${version_tag}:refs/tags/${version_tag}"
    if [ $? -ne 0 ]; then
        echo "git push tag 失败。" >&2
        exit 1
    fi
fi

push_submodules() {
    local project_root="$1"
    local force="$2"

    if [ ! -f "$project_root/.gitmodules" ]; then
        return
    fi

    echo -e "\033[36m[push-github] 检测到子模块，开始处理子模块推送...\033[0m"

    local submodule_paths=$(git config --file "$project_root/.gitmodules" --get-regexp path | awk '{print $2}')
    if [ -z "$submodule_paths" ]; then
        echo -e "\033[33m[push-github] 未解析到子模块路径，跳过。\033[0m"
        return
    fi

    for sub_path in $submodule_paths; do
        local sub_full_path="$project_root/$sub_path"
        if [ ! -d "$sub_full_path/.git" ] && [ ! -f "$sub_full_path/.git" ]; then
            echo -e "\033[33m[push-github] 子模块 $sub_path 未初始化，跳过。\033[0m"
            continue
        fi

        echo -e "\033[36m[push-github] 处理子模块: $sub_path\033[0m"

        # 进入子模块目录
        cd "$sub_full_path"

        # 检查子模块是否有本地改动需要提交
        local sub_status=$(git status --short 2>/dev/null || true)
        if [ -n "$(echo "$sub_status" | tr -d '[:space:]')" ]; then
            local sub_tracked=$(echo "$sub_status" | grep -v '^??' | wc -l)
            local sub_untracked=$(echo "$sub_status" | grep '^??' | wc -l)
            echo -e "\033[33m[push-github] 子模块 $sub_path 存在改动：已跟踪 ${sub_tracked##*( )} 个，未跟踪 ${sub_untracked##*( )} 个。\033[0m"

            read -p "是否提交子模块 $sub_path 的改动并推送？(Y/n): " sub_commit_choice
            if [ -z "$sub_commit_choice" ] || [[ "$sub_commit_choice" =~ ^(y|yes)$ ]]; then
                git add -A
                if [ $? -ne 0 ]; then
                    echo -e "\033[33m[push-github] 子模块 $sub_path git add 失败，跳过。\033[0m" >&2
                    cd "$project_root"
                    continue
                fi

                local sub_staged=$(git diff --cached --name-only 2>/dev/null || true)
                local sub_staged_count=$(echo "$sub_staged" | grep -c '.' 2>/dev/null || echo "0")
                if [ "$sub_staged_count" -gt 0 ]; then
                    read -p "请输入子模块 $sub_path 的 commit 信息: " sub_message
                    if [ -z "$(echo "$sub_message" | tr -d '[:space:]')" ]; then
                        sub_message="update submodule $sub_path"
                    fi
                    git commit -m "$sub_message"
                    if [ $? -ne 0 ]; then
                        echo -e "\033[33m[push-github] 子模块 $sub_path git commit 失败，跳过。\033[0m" >&2
                        cd "$project_root"
                        continue
                    fi
                fi
            fi
        fi

        # 推送子模块
        local sub_branch=$(git branch --show-current 2>/dev/null || true)
        if [ -z "$sub_branch" ]; then
            echo -e "\033[33m[push-github] 子模块 $sub_path 处于 detached HEAD，跳过推送。\033[0m"
            cd "$project_root"
            continue
        fi

        if [ "$force" = true ]; then
            git push --force-with-lease -u origin "$sub_branch"
        else
            git push -u origin "$sub_branch"
        fi
        if [ $? -ne 0 ]; then
            echo -e "\033[33m[push-github] 子模块 $sub_path 推送失败，请手动检查。\033[0m" >&2
            cd "$project_root"
            continue
        fi

        echo -e "\033[32m[push-github] 子模块 $sub_path 推送完成。分支: $sub_branch\033[0m"
        cd "$project_root"
    done

    echo -e "\033[32m[push-github] 子模块处理完成。\033[0m"
}

echo -e "\033[32m[push-github] 已完成推送。分支: $branch\033[0m"
if [ -n "$version_tag" ]; then
    echo -e "\033[32m[push-github] 已完成远端版本标签同步: $version_tag\033[0m"
fi

push_submodules "$PROJECT_ROOT" "$force_push"

# 子模块推送后，主仓库可能需要更新子模块引用并推送
if [ -f "$PROJECT_ROOT/.gitmodules" ]; then
    cd "$PROJECT_ROOT"
    submodules_changed=$(git diff --name-only 2>/dev/null || true)
    if [ -n "$(echo "$submodules_changed" | tr -d '[:space:]')" ]; then
        echo -e "\033[36m[push-github] 子模块引用已变更，提交并推送主仓库更新...\033[0m"
        git add -A
        git commit -m "update submodule references"
        if [ "$force_push" = true ]; then
            git push --force-with-lease -u origin "$branch"
        else
            git push -u origin "$branch"
        fi
        if [ $? -ne 0 ]; then
            echo -e "\033[33m[push-github] 主仓库子模块引用推送失败，请手动检查。\033[0m" >&2
        else
            echo -e "\033[32m[push-github] 主仓库子模块引用已推送。\033[0m"
        fi
    fi
fi

cd "$ORIGINAL_DIR"
