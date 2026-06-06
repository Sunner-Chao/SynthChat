#!/bin/bash

# GitHub 仓库创建与权限管理工具 - Linux/Mac 版本
# 创建仓库后可直接配置可见性和协作者权限

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
GRAY='\033[0;90m'
NC='\033[0m'

# 默认值
NAME=""
DESCRIPTION=""
PRIVATE=false
PROTOCOL="ssh"
SSH_HOST="github.com"
REMOTE_NAME="origin"
ORG=""
NO_SET_REMOTE=false
NO_PUSH=false
NO_CONFIG=false

# 解析参数
while [[ $# -gt 0 ]]; do
    case $1 in
        -n|--name)
            NAME="$2"
            shift 2
            ;;
        -d|--description)
            DESCRIPTION="$2"
            shift 2
            ;;
        -p|--private)
            PRIVATE=true
            shift
            ;;
        --public)
            PRIVATE=false
            shift
            ;;
        --protocol)
            PROTOCOL="$2"
            shift 2
            ;;
        --ssh-host)
            SSH_HOST="$2"
            shift 2
            ;;
        --remote)
            REMOTE_NAME="$2"
            shift 2
            ;;
        --org)
            ORG="$2"
            shift 2
            ;;
        --no-set-remote)
            NO_SET_REMOTE=true
            shift
            ;;
        --no-push)
            NO_PUSH=true
            shift
            ;;
        --no-config)
            NO_CONFIG=true
            shift
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            shift
            ;;
    esac
done

# 辅助函数
step() {
    echo ""
    echo -e "${CYAN}== $1 ==${NC}"
}

info() {
    echo -e "${GRAY}  $1${NC}"
}

success() {
    echo -e "${GREEN}  $1${NC}"
}

warn() {
    echo -e "${YELLOW}  $1${NC}"
}

error() {
    echo -e "${RED}  $1${NC}"
}

show_menu() {
    local title="$1"
    shift
    local options=("$@")
    local default="${options[-1]}"
    unset 'options[-1]'

    echo ""
    echo -e "${YELLOW}$title${NC}"
    local i=1
    for opt in "${options[@]}"; do
        if [[ $i -eq $default ]]; then
            echo -e "${WHITE}  $i. $opt ${GRAY}[默认]${NC}"
        else
            echo -e "${WHITE}  $i. $opt${NC}"
        fi
        ((i++))
    done
    echo ""
}

read_choice() {
    local prompt="$1"
    local max="$2"
    local default="$3"

    while true; do
        echo -e "${CYAN}$prompt${NC} ${GRAY}(直接回车选择 $default)${NC}"
        read -r input

        if [[ -z "$input" ]]; then
            return "$default"
        fi

        if [[ "$input" =~ ^[0-9]+$ ]] && [[ "$input" -ge 1 ]] && [[ "$input" -le "$max" ]]; then
            return "$input"
        fi

        error "输入无效，请输入 1-$max"
    done
}

get_token() {
    if [[ -n "$GITHUB_TOKEN" ]]; then
        echo "$GITHUB_TOKEN"
        return 0
    fi
    if [[ -n "$GH_TOKEN" ]]; then
        echo "$GH_TOKEN"
        return 0
    fi
    if command -v gh &> /dev/null; then
        local token
        token=$(gh auth token 2>/dev/null)
        if [[ -n "$token" ]]; then
            echo "$token"
            return 0
        fi
    fi
    return 1
}

check_auth() {
    if get_token &> /dev/null; then
        return 0
    fi
    if command -v gh &> /dev/null && gh auth status &> /dev/null; then
        return 0
    fi
    return 1
}

api_call() {
    local method="$1"
    local endpoint="$2"
    local body="$3"

    local token
    token=$(get_token) || { error "需要认证"; exit 1; }

    local url="https://api.github.com/$endpoint"
    local args=(-s -X "$method" -H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json")

    if [[ -n "$body" ]]; then
        args+=(-H "Content-Type: application/json" -d "$body")
    fi

    curl "${args[@]}" "$url"
}

# ===== 仓库创建函数 =====

create_with_gh() {
    local target="$1"
    local desc="$2"
    local is_private="$3"

    if ! command -v gh &> /dev/null || ! gh auth status &> /dev/null; then
        return 1
    fi

    local args=(repo create "$target")

    if [[ "$is_private" == "true" ]]; then
        args+=(--private)
    else
        args+=(--public)
    fi

    if [[ -n "$desc" ]]; then
        args+=(--description "$desc")
    fi

    step "使用 gh 创建 GitHub 仓库"
    info "目标仓库: $target"

    gh "${args[@]}" 2>&1 || true

    local repo_json
    repo_json=$(gh repo view "$target" --json url,sshUrl,nameWithOwner,visibility 2>/dev/null)

    if [[ -n "$repo_json" ]]; then
        echo "$repo_json"
        return 0
    fi

    return 1
}

create_with_api() {
    local name="$1"
    local desc="$2"
    local is_private="$3"
    local org="$4"

    local body="{\"name\":\"$name\",\"description\":\"$desc\",\"private\":$is_private}"

    local uri
    if [[ -n "$org" ]]; then
        uri="orgs/$org/repos"
        step "通过 GitHub API 创建组织仓库"
        info "组织: $org"
    else
        uri="user/repos"
        step "通过 GitHub API 创建个人仓库"
    fi

    local result
    result=$(api_call "POST" "$uri" "$body")

    echo "$result"
}

get_repo_info_from_json() {
    local json="$1"
    python3 -c "
import sys, json
data = json.load(sys.stdin)
print(f\"{data.get('nameWithOwner', data.get('full_name', ''))}|{data.get('url', data.get('html_url', ''))}|{data.get('sshUrl', data.get('ssh_url', ''))}|{data.get('visibility', 'public' if not data.get('private', False) else 'private')}\")
" 2>/dev/null
}

ensure_remote() {
    local remote_url="$1"

    if [[ "$NO_SET_REMOTE" == "true" ]]; then
        step "远程绑定"
        warn "已跳过远程绑定"
        return
    fi

    local existing
    existing=$(git remote get-url "$REMOTE_NAME" 2>/dev/null) || true

    step "配置 Git 远程"
    if [[ -n "$existing" ]]; then
        info "更新远程 $REMOTE_NAME -> $remote_url"
        git remote set-url "$REMOTE_NAME" "$remote_url"
    else
        info "新增远程 $REMOTE_NAME -> $remote_url"
        git remote add "$REMOTE_NAME" "$remote_url"
    fi
}

ensure_initial_push() {
    local repo="$1"

    if [[ "$NO_PUSH" == "true" ]]; then
        step "初次推送"
        warn "已跳过初次推送"
        return
    fi

    local branch
    branch=$(git branch --show-current 2>/dev/null) || return

    if [[ -z "$branch" ]]; then
        warn "未检测到当前分支"
        return
    fi

    local status
    status=$(git status --porcelain 2>/dev/null)
    if [[ -n "$status" ]]; then
        step "初次推送"
        warn "当前工作区有未提交改动，已跳过推送"
        return
    fi

    step "初次推送"
    info "推送当前分支到远程..."
    git push -u "$REMOTE_NAME" "$branch"
}

# ===== 权限管理函数 =====

set_visibility() {
    local repo="$1"

    step "设置仓库可见性"

    local current
    current=$(gh repo view "$repo" --json visibility 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('visibility','public'))" 2>/dev/null) || current="public"

    warn "当前可见性: $current"

    local default_num
    if [[ "$current" == "private" ]]; then
        default_num=1
    else
        default_num=2
    fi

    show_menu "选择新可见性" \
        "public - 公开" \
        "private - 私有" \
        "$default_num"

    read_choice "请选择" 2 "$default_num"
    local choice=$?

    local new_visibility
    if [[ $choice -eq 1 ]]; then
        new_visibility="public"
    else
        new_visibility="private"
    fi

    if [[ "$new_visibility" == "$current" ]]; then
        info "可见性未变化"
        echo "$current"
        return
    fi

    info "即将设置为: $new_visibility"
    echo -e "${CYAN}确认修改？${NC} ${GRAY}(直接回车确认)${NC}"
    read -r confirm
    if [[ -n "$confirm" ]] && [[ ! "$confirm" =~ ^(y|yes)$ ]]; then
        warn "已取消"
        echo "$current"
        return
    fi

    if command -v gh &> /dev/null && gh auth status &> /dev/null; then
        gh repo edit "$repo" --visibility "$new_visibility" --accept-visibility-change-consequences 2>&1 && {
            success "已设置为 $new_visibility"
            echo "$new_visibility"
            return
        }
    fi

    local body="{\"private\":$(if [[ "$new_visibility" == "private" ]]; then echo "true"; else echo "false"; fi)}"
    api_call "PATCH" "repos/$repo" "$body" > /dev/null
    success "已设置为 $new_visibility"
    echo "$new_visibility"
}

get_collaborators() {
    local repo="$1"
    api_call "GET" "repos/$repo/collaborators?affiliation=direct"
}

show_collaborators() {
    local repo="$1"

    step "当前协作者列表"

    local collabs_json
    collabs_json=$(get_collaborators "$repo") || { info "无法获取"; return 0; }

    if [[ -z "$collabs_json" ]] || [[ "$collabs_json" == "[]" ]]; then
        info "无外部协作者"
        return 0
    fi

    echo "$collabs_json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
i = 1
for c in data:
    perms = c.get('permissions', {})
    main = 'admin' if perms.get('admin') else \
           'maintain' if perms.get('maintain') else \
           'write' if perms.get('write') else \
           'triage' if perms.get('triage') else \
           'read' if perms.get('read') else 'unknown'
    print(f'{i}|{c[\"login\"]}|{main}')
    i += 1
" 2>/dev/null | while IFS='|' read -r idx login perm; do
        echo -e "${WHITE}  $idx. $login [$perm]${NC}"
    done

    local count
    count=$(echo "$collabs_json" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo 0)
    return "$count"
}

add_collaborator() {
    local repo="$1"
    local username="$2"
    local permission="$3"

    local body="{\"permission\":\"$permission\"}"
    api_call "PUT" "repos/$repo/collaborators/$username" "$body" > /dev/null
    success "已邀请 $username，权限: $permission"
    info "(协作者需要接受邀请才能访问仓库)"
}

remove_collaborator() {
    local repo="$1"
    local username="$2"

    api_call "DELETE" "repos/$repo/collaborators/$username" > /dev/null
    success "已移除 $username"
}

manage_collaborators() {
    local repo="$1"

    step "管理协作者权限"

    show_collaborators "$repo"
    local collab_count=$?

    show_menu "操作类型" \
        "添加协作者" \
        "移除协作者" \
        "返回上级" \
        1

    read_choice "请选择操作" 3 1
    local action=$?

    if [[ $action -eq 3 ]]; then return; fi

    if [[ $action -eq 2 ]]; then
        if [[ $collab_count -eq 0 ]]; then
            warn "无协作者可移除"
            return
        fi

        echo ""
        warn "选择要移除的协作者"

        local usernames=()
        usernames=($(get_collaborators "$repo" | python3 -c "import sys,json; [print(c['login']) for c in json.load(sys.stdin)]" 2>/dev/null))

        local total=${#usernames[@]}
        if [[ $total -eq 0 ]]; then
            warn "无法解析列表"
            return
        fi

        local i=1
        for u in "${usernames[@]}"; do
            echo -e "${WHITE}  $i. $u${NC}"
            ((i++))
        done
        echo -e "${GRAY}  $i. 返回上级${NC}"

        read_choice "请选择" "$i" "$i"
        local select=$?

        if [[ $select -eq $i ]]; then return; fi

        local target="${usernames[$select-1]}"
        info "即将移除: $target"

        echo -e "${CYAN}确认移除？${NC} ${GRAY}(直接回车确认)${NC}"
        read -r confirm
        if [[ -n "$confirm" ]] && [[ ! "$confirm" =~ ^(y|yes)$ ]]; then
            warn "已取消"
            return
        fi

        remove_collaborator "$repo" "$target"
    else
        echo -e "${CYAN}请输入协作者用户名 (GitHub 用户名)${NC}"
        read -r username

        if [[ -z "$username" ]]; then
            warn "已取消"
            return
        fi

        show_menu "选择权限级别" \
            "admin    - 完全管理权限" \
            "maintain - 维护权限" \
            "write    - 写入权限" \
            "triage   - 分类权限" \
            "read     - 只读权限" \
            "取消添加" \
            3

        read_choice "请选择权限" 6 3
        local perm_choice=$?

        if [[ $perm_choice -eq 6 ]]; then
            warn "已取消"
            return
        fi

        local permissions=("admin" "maintain" "write" "triage" "read")
        local new_perm="${permissions[$perm_choice-1]}"

        info "即将设置: $username -> $new_perm"
        echo -e "${CYAN}确认添加？${NC} ${GRAY}(直接回车确认)${NC}"
        read -r confirm
        if [[ -n "$confirm" ]] && [[ ! "$confirm" =~ ^(y|yes)$ ]]; then
            warn "已取消"
            return
        fi

        add_collaborator "$repo" "$username" "$new_perm"
    fi
}

show_repo_status() {
    local repo="$1"
    local visibility="$2"

    step "仓库状态"
    warn "仓库: $repo"
    warn "可见性: $visibility"
    echo ""

    show_collaborators "$repo"
}

configure_permissions() {
    local repo="$1"
    local initial_visibility="$2"

    echo ""
    echo -e "${CYAN}仓库创建成功！是否需要配置权限？${NC}"

    show_menu "权限配置" \
        "进入权限配置菜单" \
        "跳过配置，完成退出" \
        1

    read_choice "请选择" 2 1
    local config_choice=$?

    if [[ $config_choice -eq 2 ]]; then
        echo "$initial_visibility"
        return
    fi

    local current_visibility="$initial_visibility"

    while true; do
        show_menu "权限配置菜单" \
            "查看当前状态" \
            "设置仓库可见性" \
            "管理协作者权限" \
            "完成配置" \
            4

        read_choice "请选择操作" 4 4
        local choice=$?

        case $choice in
            1) show_repo_status "$repo" "$current_visibility" ;;
            2) current_visibility=$(set_visibility "$repo") ;;
            3) manage_collaborators "$repo" ;;
            4)
                echo ""
                success "权限配置完成"
                echo "$current_visibility"
                return
                ;;
        esac
    done
}

show_help() {
    cat << EOF
GitHub 仓库创建与权限管理工具

用法:
  ./create-github-repo.sh [选项]

选项:
  -n, --name <名称>       仓库名称
  -d, --description <描述> 仓库描述
  -p, --private           创建私有仓库
  --public                创建公开仓库
  --protocol <ssh|https>  远程协议 (默认 ssh)
  --ssh-host <host>       SSH Host 别名
  --remote <名称>         远程名称 (默认 origin)
  --org <组织>            创建组织仓库
  --no-set-remote         跳过设置远程
  --no-push               跳过初次推送
  --no-config             跳过权限配置
  -h, --help              显示帮助

交互式运行（无参数时自动询问）:
  ./create-github-repo.sh

创建仓库后会自动进入权限配置菜单，
可设置可见性、添加/移除协作者。

认证:
  需要先运行 'gh auth login' 或设置:
  export GITHUB_TOKEN='your_token'
EOF
}

# ===== 交互式询问 =====

ask_interactive() {
    if [[ -z "$NAME" ]]; then
        echo -e "${CYAN}请输入仓库名称 (Name)${NC}"
        read -r NAME
        if [[ -z "$NAME" ]]; then
            error "仓库名称不能为空"
            exit 1
        fi
    fi

    # 询问可见性
    if [[ "$PRIVATE" != "true" ]] && [[ -z "$PS_PRIVATE" ]]; then
        show_menu "选择仓库可见性" \
            "public - 公开" \
            "private - 私有" \
            1

        read_choice "请选择" 2 1
        local vis_choice=$?
        if [[ $vis_choice -eq 2 ]]; then
            PRIVATE=true
        fi
    fi

    # 询问描述
    if [[ -z "$DESCRIPTION" ]]; then
        echo -e "${CYAN}请输入仓库描述 (可选，直接回车跳过)${NC}"
        read -r DESCRIPTION
    fi

    # SSH Host 别名
    if [[ "$PROTOCOL" == "ssh" ]] && [[ "$SSH_HOST" == "github.com" ]]; then
        echo -e "${CYAN}是否使用 SSH Host 别名？${NC} ${GRAY}(y/N)${NC}"
        read -r use_alias
        if [[ "$use_alias" =~ ^(y|yes)$ ]]; then
            echo -e "${CYAN}请输入 SSH Host 别名${NC} ${GRAY}(直接回车默认 github-sunner)${NC}"
            read -r alias_input
            if [[ -n "$alias_input" ]]; then
                SSH_HOST="$alias_input"
            else
                SSH_HOST="github-sunner"
            fi
        fi
    fi
}

# ===== 主流程 =====

echo ""
echo -e "${CYAN}GitHub 仓库创建工具${NC}"
echo -e "${CYAN}====================${NC}"

if ! check_auth; then
    echo ""
    error "[警告] 未检测到 GitHub 认证"
    warn "请先运行: gh auth login"
    warn "或设置: export GITHUB_TOKEN='your_token'"
    echo ""
    echo -e "${CYAN}是否继续尝试？${NC} ${GRAY}(直接回车退出)${NC}"
    read -r continue
    if [[ -z "$continue" ]]; then
        exit 1
    fi
fi

# 交互式询问
ask_interactive

echo ""
info "仓库名称: $NAME"
info "可见性: $(if [[ "$PRIVATE" == "true" ]]; then echo "private"; else echo "public"; fi)"
info "描述: $(if [[ -n "$DESCRIPTION" ]]; then echo "$DESCRIPTION"; else echo "无"; fi)"
if [[ -n "$ORG" ]]; then info "组织: $ORG"; fi

# 创建仓库
target="${NAME}"
if [[ -n "$ORG" ]]; then
    target="${ORG}/${NAME}"
fi

repo_json=""
try_gh=true

if command -v gh &> /dev/null && gh auth status &> /dev/null; then
    repo_json=$(create_with_gh "$target" "$DESCRIPTION" "$PRIVATE") && try_gh=false || try_gh=true
fi

if [[ "$try_gh" == "true" ]] || [[ -z "$repo_json" ]]; then
    if [[ -n "$ORG" ]]; then
        repo_json=$(create_with_api "$NAME" "$DESCRIPTION" "$PRIVATE" "$ORG")
    else
        repo_json=$(create_with_api "$NAME" "$DESCRIPTION" "$PRIVATE" "")
    fi
fi

# 解析仓库信息
repo_info=$(get_repo_info_from_json "$repo_json")
IFS='|' read -r repo_name html_url ssh_url visibility <<< "$repo_info"

# 构建 remote URL
remote_url=""
if [[ "$PROTOCOL" == "ssh" ]]; then
    if [[ "$SSH_HOST" != "github.com" ]] && [[ "$ssh_url" =~ git@github\.com:(.+)$ ]]; then
        remote_url="git@${SSH_HOST}:${BASH_REMATCH[1]}"
    else
        remote_url="$ssh_url"
    fi
else
    remote_url="$html_url.git"
fi

ensure_remote "$remote_url"
ensure_initial_push "$repo_name"

echo ""
success "远程仓库已就绪"
warn "仓库:    $repo_name"
warn "网页:    $html_url"
warn "远程:    $remote_url"

# 权限配置
if [[ "$NO_CONFIG" != "true" ]]; then
    final_visibility=$(configure_permissions "$repo_name" "$visibility")
fi

echo ""
success "完成！"
echo -e "${CYAN}后续常用命令：${NC}"
info "git status"
info 'git add . && git commit -m "message"'
info "git push"