param(
    [string]$Name,

    [string]$Description = '',

    [switch]$Private,

    [ValidateSet('ssh', 'https')]
    [string]$Protocol = 'ssh',

    [string]$SshHost = 'github.com',

    [string]$RemoteName = 'origin',

    [switch]$NoSetRemote,

    [switch]$NoPush,

    [string]$Org,

    [switch]$OpenRepo,

    [switch]$NoConfig  # 跳过权限配置
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

# 加载共享模块
$helperModule = Join-Path $ProjectRoot 'git-remote-helper.ps1'
. $helperModule

# ===== GitHub 认证函数 =====

function Get-GitHubToken {
    if ($env:GITHUB_TOKEN) { return $env:GITHUB_TOKEN }
    if ($env:GH_TOKEN) { return $env:GH_TOKEN }
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if ($gh) {
        $token = (& gh auth token 2>$null).Trim()
        if ($token) { return $token }
    }
    return $null
}

function Check-Auth {
    $token = Get-GitHubToken
    if ($token) { return $true }
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if ($gh) {
        $status = (& gh auth status 2>&1)
        if ($LASTEXITCODE -eq 0) { return $true }
    }
    return $false
}

# ===== 仓库创建函数 =====

function Get-RepoVisibility {
    if ($Private) { return 'private' }
    return 'public'
}

function Create-WithGh {
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if (-not $gh) { return $null }

    $target = if ($Org) { "$Org/$Name" } else { $Name }
    $args = @('repo', 'create', $target)

    if ($Private) {
        $args += '--private'
    } else {
        $args += '--public'
    }

    if ($Description) {
        $args += @('--description', $Description)
    }

    Write-Step "使用 gh 创建 GitHub 仓库"
    Write-Info "目标仓库: $target"
    & gh @args 2>&1 | Tee-Object -Variable ghOutput | Out-Host
    if ($LASTEXITCODE -ne 0) {
        $ghText = ($ghOutput | Out-String)
        if ($ghText -match 'Name already exists on this account') {
            Write-Host "  仓库已存在，改为接管已有仓库。" -ForegroundColor Yellow
        } else {
            throw "gh repo create 失败。"
        }
    }

    $repoJson = & gh repo view $target --json url,sshUrl,nameWithOwner,visibility 2>$null | ConvertFrom-Json
    if (-not $repoJson) { return $null }

    return @{
        NameWithOwner = $repoJson.nameWithOwner
        HtmlUrl       = $repoJson.url
        SshUrl        = $repoJson.sshUrl
        HttpsUrl      = "https://github.com/$($repoJson.nameWithOwner).git"
        Visibility    = $repoJson.visibility
    }
}

function Create-WithApi {
    $token = Get-GitHubToken
    if (-not $token) {
        throw "需要 GitHub 认证。请运行 gh auth login 或设置 GITHUB_TOKEN 环境变量。"
    }

    $headers = @{
        Authorization = "Bearer $token"
        Accept = 'application/vnd.github+json'
        'User-Agent' = 'git-shell-scripts'
    }

    $body = @{
        name        = $Name
        description = $Description
        private     = [bool]$Private
    }

    if ($Org) {
        $uri = "https://api.github.com/orgs/$Org/repos"
        Write-Step "通过 GitHub API 创建组织仓库"
        Write-Info "组织: $Org"
    } else {
        $uri = 'https://api.github.com/user/repos'
        Write-Step "通过 GitHub API 创建个人仓库"
    }

    try {
        $repo = Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -Body ($body | ConvertTo-Json)
    } catch {
        $errorMessage = $_.Exception.Message
        if ($errorMessage -match '422' -or $errorMessage -match 'already exists') {
            Write-Host "  仓库已存在，改为读取已有仓库信息。" -ForegroundColor Yellow
            $repoPath = if ($Org) { "$Org/$Name" } else {
                $viewer = Invoke-RestMethod -Method Get -Uri 'https://api.github.com/user' -Headers $headers
                "$($viewer.login)/$Name"
            }
            $repo = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$repoPath" -Headers $headers
        } else {
            throw
        }
    }

    return @{
        NameWithOwner = $repo.full_name
        HtmlUrl       = $repo.html_url
        SshUrl        = $repo.ssh_url
        HttpsUrl      = $repo.clone_url
        DefaultBranch = $repo.default_branch
        Visibility    = if ($repo.private) { 'private' } else { 'public' }
    }
}

function Ensure-InitialPush {
    param([string]$RepoNameWithOwner)
    if ($NoPush) {
        Write-Step "初次推送"
        Write-Host "  已跳过初次推送（NoPush）。" -ForegroundColor Yellow
        return
    }

    $branch = (& git branch --show-current).Trim()
    if (-not $branch) { throw "未检测到当前分支，无法推送到 $RepoNameWithOwner。" }

    $status = & git status --porcelain
    if ($status) {
        Write-Step "初次推送"
        Write-Warning "当前工作区仍有未提交改动，已跳过自动推送。请先提交后再执行推送。"
        return
    }

    Write-Step "初次推送"
    Write-Host "  推送当前分支到远程..." -ForegroundColor Cyan
    & git push -u $RemoteName $branch
    if ($LASTEXITCODE -ne 0) { throw "git push 失败。" }
}

# ===== 权限管理函数 =====

function Set-Visibility {
    param([string]$RepoName)

    Write-Step "设置仓库可见性"

    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if ($gh) {
        $currentJson = & gh repo view $RepoName --json visibility 2>$null | ConvertFrom-Json
        $current = $currentJson.visibility
    } else {
        $token = Get-GitHubToken
        $headers = @{
            Authorization = "Bearer $token"
            Accept = "application/vnd.github+json"
        }
        $repoInfo = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$RepoName" -Headers $headers
        $current = if ($repoInfo.private) { 'private' } else { 'public' }
    }

    Write-Host "  当前可见性: $current" -ForegroundColor Yellow

    $defaultNum = if ($current -eq "private") { 1 } else { 2 }
    Write-Menu "选择新可见性" @("public - 公开", "private - 私有") -Default $defaultNum

    $choice = Read-MenuChoice -Prompt "请选择" -Max 2 -Default $defaultNum
    $newVisibility = if ($choice -eq 1) { "public" } else { "private" }

    if ($newVisibility -eq $current) {
        Write-Host "  可见性未变化，无需修改" -ForegroundColor Gray
        return $current
    }

    Write-Host "  即将设置为: $newVisibility" -ForegroundColor Cyan
    $confirm = Read-Host "确认修改？(直接回车确认)"
    if (-not [string]::IsNullOrWhiteSpace($confirm) -and $confirm -notmatch '^(y|yes)$') {
        Write-Host "  已取消" -ForegroundColor Yellow
        return $current
    }

    if ($gh) {
        & gh repo edit $RepoName --visibility $newVisibility --accept-visibility-change-consequences 2>&1 | Out-Host
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  已设置为 $newVisibility" -ForegroundColor Green
            return $newVisibility
        }
    }

    $token = Get-GitHubToken
    $headers = @{
        Authorization = "Bearer $token"
        Accept = "application/vnd.github+json"
    }
    $body = @{ private = ($newVisibility -eq "private") }
    Invoke-RestMethod -Method Patch -Uri "https://api.github.com/repos/$RepoName" -Headers $headers -Body ($body | ConvertTo-Json) | Out-Null
    Write-Host "  已设置为 $newVisibility" -ForegroundColor Green
    return $newVisibility
}

function Get-Collaborators {
    param([string]$RepoName)

    $token = Get-GitHubToken
    if (-not $token) { throw "需要认证" }

    $headers = @{
        Authorization = "Bearer $token"
        Accept = "application/vnd.github+json"
    }

    return Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$RepoName/collaborators?affiliation=direct" -Headers $headers
}

function Show-Collaborators {
    param([string]$RepoName)

    Write-Step "当前协作者列表"

    try {
        $collabs = Get-Collaborators -RepoName $RepoName
    } catch {
        Write-Host "  无法获取协作者列表" -ForegroundColor Gray
        return @()
    }

    if (-not $collabs -or $collabs.Count -eq 0) {
        Write-Host "  无外部协作者" -ForegroundColor Gray
        return @()
    }

    $list = @()
    $index = 1
    foreach ($c in $collabs) {
        $perms = $c.permissions
        $mainPerm = if ($perms.admin) { "admin" }
                    elseif ($perms.maintain) { "maintain" }
                    elseif ($perms.write) { "write" }
                    elseif ($perms.triage) { "triage" }
                    elseif ($perms.read) { "read" }
                    else { "unknown" }

        Write-Host "  $index. $($c.login) [$mainPerm]" -ForegroundColor White
        $list += @{ login = $c.login; permission = $mainPerm }
        $index++
    }

    return $list
}

function Add-Collaborator {
    param([string]$RepoName, [string]$Username, [string]$Permission)

    $token = Get-GitHubToken
    $headers = @{
        Authorization = "Bearer $token"
        Accept = "application/vnd.github+json"
    }
    $body = @{ permission = $Permission }
    Invoke-RestMethod -Method Put -Uri "https://api.github.com/repos/$RepoName/collaborators/$Username" -Headers $headers -Body ($body | ConvertTo-Json) | Out-Null
    Write-Host "  已邀请 $Username，权限: $Permission" -ForegroundColor Green
    Write-Host "  (协作者需要接受邀请才能访问仓库)" -ForegroundColor DarkGray
}

function Remove-Collaborator {
    param([string]$RepoName, [string]$Username)

    $token = Get-GitHubToken
    $headers = @{
        Authorization = "Bearer $token"
        Accept = "application/vnd.github+json"
    }
    Invoke-RestMethod -Method Delete -Uri "https://api.github.com/repos/$RepoName/collaborators/$Username" -Headers $headers | Out-Null
    Write-Host "  已移除 $Username" -ForegroundColor Green
}

function Manage-Collaborators {
    param([string]$RepoName)

    Write-Step "管理协作者权限"

    $existingCollabs = Show-Collaborators -RepoName $RepoName

    Write-Menu "操作类型" @("添加协作者", "移除协作者", "返回上级") -Default 1
    $actionChoice = Read-MenuChoice -Prompt "请选择操作" -Max 3 -Default 1

    if ($actionChoice -eq 3) { return }

    if ($actionChoice -eq 2) {
        if ($existingCollabs.Count -eq 0) {
            Write-Host "  无协作者可移除" -ForegroundColor Yellow
            return
        }

        Write-Host ""
        Write-Host "选择要移除的协作者" -ForegroundColor Yellow
        for ($i = 0; $i -lt $existingCollabs.Count; $i++) {
            Write-Host "  $($i + 1). $($existingCollabs[$i].login)" -ForegroundColor White
        }
        Write-Host "  $($existingCollabs.Count + 1). 返回上级" -ForegroundColor Gray

        $select = Read-MenuChoice -Prompt "请选择" -Max ($existingCollabs.Count + 1) -Default ($existingCollabs.Count + 1)
        if ($select -eq $existingCollabs.Count + 1) { return }

        $targetUser = $existingCollabs[$select - 1].login

        Write-Host "  即将移除: $targetUser" -ForegroundColor Cyan
        $confirm = Read-Host "确认移除？(直接回车确认)"
        if (-not [string]::IsNullOrWhiteSpace($confirm) -and $confirm -notmatch '^(y|yes)$') {
            Write-Host "  已取消" -ForegroundColor Yellow
            return
        }

        Remove-Collaborator -RepoName $RepoName -Username $targetUser
    } else {
        $username = Read-Host "请输入协作者用户名 (GitHub 用户名)"
        if ([string]::IsNullOrWhiteSpace($username)) {
            Write-Host "  已取消" -ForegroundColor Yellow
            return
        }
        $username = $username.Trim()

        $existing = $existingCollabs | Where-Object { $_.login -eq $username }
        if ($existing) {
            Write-Host "  用户已存在，当前权限: $($existing.permission)" -ForegroundColor Yellow
        }

        Write-Menu "选择权限级别" @(
            "admin    - 完全管理权限",
            "maintain - 维护权限",
            "write    - 写入权限",
            "triage   - 分类权限",
            "read     - 只读权限",
            "取消添加"
        ) -Default 3

        $permChoice = Read-MenuChoice -Prompt "请选择权限" -Max 6 -Default 3
        if ($permChoice -eq 6) {
            Write-Host "  已取消" -ForegroundColor Yellow
            return
        }

        $permissions = @("admin", "maintain", "write", "triage", "read")
        $newPermission = $permissions[$permChoice - 1]

        Write-Host "  即将设置: $username -> $newPermission" -ForegroundColor Cyan
        $confirm = Read-Host "确认添加？(直接回车确认)"
        if (-not [string]::IsNullOrWhiteSpace($confirm) -and $confirm -notmatch '^(y|yes)$') {
            Write-Host "  已取消" -ForegroundColor Yellow
            return
        }

        Add-Collaborator -RepoName $RepoName -Username $username -Permission $newPermission
    }
}

function Show-RepoStatus {
    param([string]$RepoName, [string]$Visibility)

    Write-Step "仓库状态"

    Write-Host "  仓库: $RepoName" -ForegroundColor Yellow
    Write-Host "  可见性: $Visibility" -ForegroundColor Yellow
    Write-Host ""

    Show-Collaborators -RepoName $RepoName | Out-Null
}

function Configure-Permissions {
    param([string]$RepoName, [string]$InitialVisibility)

    Write-Host ""
    Write-Host "仓库创建成功！是否需要配置权限？" -ForegroundColor Cyan

    Write-Menu "权限配置" @(
        "进入权限配置菜单",
        "跳过配置，完成退出"
    ) -Default 1

    $configChoice = Read-MenuChoice -Prompt "请选择" -Max 2 -Default 1
    if ($configChoice -eq 2) {
        return $InitialVisibility
    }

    $currentVisibility = $InitialVisibility

    while ($true) {
        Write-Host ""
        Write-Menu "权限配置菜单" @(
            "查看当前状态",
            "设置仓库可见性",
            "管理协作者权限",
            "完成配置"
        ) -Default 4

        $choice = Read-MenuChoice -Prompt "请选择操作" -Max 4 -Default 4

        switch ($choice) {
            1 { Show-RepoStatus -RepoName $RepoName -Visibility $currentVisibility }
            2 { $currentVisibility = Set-Visibility -RepoName $RepoName }
            3 { Manage-Collaborators -RepoName $RepoName }
            4 {
                Write-Host ""
                Write-Host "权限配置完成" -ForegroundColor Green
                return $currentVisibility
            }
        }
    }
}

# ===== SSH Host 别名询问 =====

if (-not $Name) {
    $Name = Read-Host "请输入仓库名称 (Name)"
}
if (-not $Name) { throw "Name 不能为空。" }

if ($Protocol -eq 'ssh' -and -not $PSBoundParameters.ContainsKey('SshHost')) {
    $SshHost = Get-SshHost -DefaultAlias 'github-sunner'
}

# ===== 询问可见性（如果没有指定） =====

if (-not $PSBoundParameters.ContainsKey('Private')) {
    Write-Menu "选择仓库可见性" @("public - 公开", "private - 私有") -Default 1
    $visChoice = Read-MenuChoice -Prompt "请选择" -Max 2 -Default 1
    $Private = ($visChoice -eq 2)
}

# ===== 询问描述（如果没有指定） =====

if (-not $Description) {
    $descInput = Read-Host "请输入仓库描述 (可选，直接回车跳过)"
    if (-not [string]::IsNullOrWhiteSpace($descInput)) {
        $Description = $descInput.Trim()
    }
}

# ===== 主流程 =====

Write-Host ""
Write-Host "GitHub 仓库创建工具" -ForegroundColor Cyan
Write-Host "====================" -ForegroundColor Cyan

if (-not (Check-Auth)) {
    Write-Host ""
    Write-Host "[警告] 未检测到 GitHub 认证" -ForegroundColor Red
    Write-Host "请先运行: gh auth login" -ForegroundColor Yellow
    Write-Host "或设置: `$env:GITHUB_TOKEN = 'your_token'" -ForegroundColor Yellow
    Write-Host ""
    $continue = Read-Host "是否继续尝试？(直接回车退出)"
    if ([string]::IsNullOrWhiteSpace($continue)) { exit 1 }
}

Write-Host ""
Write-Info "仓库名称: $Name"
Write-Info "可见性: $(Get-RepoVisibility)"
Write-Info "描述: $(if ($Description) { $Description } else { '无' })"
if ($Org) { Write-Info "组织: $Org" }

# 创建仓库
$repoInfo = $null
try {
    $repoInfo = Create-WithGh
    if (-not $repoInfo) {
        Write-Step "回退到 GitHub API" "Yellow"
        Write-Info "未检测到可用 gh，尝试使用 API"
        $repoInfo = Create-WithApi
    }
} catch {
    Write-Warning $_.Exception.Message
    Write-Step "回退到 GitHub API" "Yellow"
    $repoInfo = Create-WithApi
}

if (-not $repoInfo) { throw "创建仓库失败。" }

# 使用共享模块解析远程 URL 并设置远程
$remoteUrl = Resolve-GitRemoteUrl -SshUrl $repoInfo.SshUrl -HttpsUrl $repoInfo.HttpsUrl -Protocol $Protocol -SshHost $SshHost

if ($NoSetRemote) {
    Write-Step "远程绑定"
    Write-Host "  已跳过远程绑定（NoSetRemote）。" -ForegroundColor Yellow
} else {
    Ensure-GitRemote -RemoteName $RemoteName -RemoteUrl $remoteUrl
}

Ensure-InitialPush -RepoNameWithOwner $repoInfo.NameWithOwner

Write-Host ""
Write-Host "远程仓库已就绪" -ForegroundColor Green
Write-Host "  仓库:    $($repoInfo.NameWithOwner)" -ForegroundColor Yellow
Write-Host "  网页:    $($repoInfo.HtmlUrl)" -ForegroundColor Yellow
Write-Host "  远程:    $remoteUrl" -ForegroundColor Yellow

# 权限配置
if (-not $NoConfig) {
    $finalVisibility = Configure-Permissions -RepoName $repoInfo.NameWithOwner -InitialVisibility $repoInfo.Visibility
}

Write-Host ""
Write-Host "完成！" -ForegroundColor Green
Write-Host "后续常用命令：" -ForegroundColor Cyan
Write-Host '  git status' -ForegroundColor DarkGray
Write-Host '  git add . && git commit -m "message"' -ForegroundColor DarkGray
Write-Host '  git push' -ForegroundColor DarkGray

if ($OpenRepo) {
    Start-Process $repoInfo.HtmlUrl
}
