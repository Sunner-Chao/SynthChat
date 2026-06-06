param(
    [string]$UserName,
    [string]$Email,
    [switch]$GlobalAccount
)

$ErrorActionPreference = 'Stop'

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$OriginalLocation = Get-Location
$ProjectRoot = $ScriptRoot
if ((Split-Path -Leaf $ScriptRoot) -ieq 'git_shell') {
    $ProjectRoot = Split-Path -Parent $ScriptRoot
}

. (Join-Path $ScriptRoot 'git-script-profile.ps1')

function Write-Step {
    param([string]$Title)
    Write-Host ""
    Write-Host "== $Title ==" -ForegroundColor Cyan
}

function Write-Ok {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor Green
}

function Write-WarnLine {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor Yellow
}

function Write-InfoLine {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor DarkGray
}

function Resolve-RepositoryUrlInteractive {
    $profileDefaults = Get-GitScriptProfile

    if ($profileDefaults.RemoteUrl) {
        $useSaved = Read-Host "检测到已保存的默认仓库地址，是否直接使用？(Y/n)"
        if (-not $useSaved -or $useSaved -match '^(y|yes)$') {
            return @{
                Repository = $profileDefaults.Repository
                RemoteUrl  = $profileDefaults.RemoteUrl
                Protocol   = $profileDefaults.Protocol
                SshHost    = $profileDefaults.SshHost
                RemoteName = $profileDefaults.RemoteName
            }
        }
    }

    $repository = Read-Host "请输入 Repository (owner/repo)"
    if (-not $repository -or -not $repository.Trim()) {
        throw "Repository 不能为空。"
    }

    if ($repository -notmatch '^[^/]+/[^/]+$') {
        throw "Repository 格式必须是 owner/repo，例如 Sunner-Chao/LStwinHR-dev。"
    }

    $protocol = Read-Host "请选择协议 ssh/https（直接回车默认 ssh）"
    if (-not $protocol) {
        $protocol = 'ssh'
    }
    $protocol = $protocol.Trim().ToLowerInvariant()
    if ($protocol -notin @('ssh', 'https')) {
        throw "协议必须是 ssh 或 https。"
    }

    $sshHost = 'github.com'
    if ($protocol -eq 'ssh') {
        $useAlias = Read-Host "是否使用 SSH Host 别名？(y/N)"
        if ($useAlias -match '^(y|yes)$') {
            $aliasInput = Read-Host "请输入 SSH Host 别名（直接回车默认 github-sunner）"
            if ([string]::IsNullOrWhiteSpace($aliasInput)) {
                $sshHost = 'github-sunner'
            } else {
                $sshHost = $aliasInput.Trim()
            }
        }
    }

    $remoteUrl = if ($protocol -eq 'https') {
        "https://github.com/$repository.git"
    } else {
        "git@${sshHost}:$repository.git"
    }

    return @{
        Repository = $repository
        RemoteUrl  = $remoteUrl
        Protocol   = $protocol
        SshHost    = $sshHost
        RemoteName = 'origin'
    }
}

function Resolve-InitialBranchName {
    $branchInput = Read-Host "请选择初始化默认分支名（直接回车默认 main）"
    if (-not $branchInput -or -not $branchInput.Trim()) {
        return 'main'
    }

    $branchName = $branchInput.Trim()
    if ($branchName -match '\s') {
        throw "分支名不能包含空白字符。"
    }

    return $branchName
}

try {
    Set-Location $ProjectRoot

    Write-Step "检查 Git"
    $gitCmd = Get-Command git -ErrorAction SilentlyContinue
    if (-not $gitCmd) {
        throw "未检测到 Git。请先安装 Git 后重试。"
    }
    Write-Ok "Git 已安装: $((& git --version).Trim())"

    Write-Step "检查当前目录"
    Write-Ok "当前项目目录: $ProjectRoot"
    $gitDirPath = Join-Path $ProjectRoot '.git'
    $isRepo = Test-Path $gitDirPath
    if ($isRepo) {
        Write-Ok "当前目录已经是 Git 仓库"

        $repoRootOutput = & git rev-parse --show-toplevel 2>$null
        $repoRoot = if ($repoRootOutput) { $repoRootOutput.Trim() } else { '' }
        $gitDirOutput = & git rev-parse --git-dir 2>$null
        $realGitDir = if ($gitDirOutput) { $gitDirOutput.Trim() } else { '' }
        $branchOutput = & git branch --show-current 2>$null
        $branch = if ($branchOutput) { $branchOutput.Trim() } else { '' }
        $upstreamOutput = & git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>$null
        $upstream = if ($upstreamOutput) { $upstreamOutput.Trim() } else { '' }
        $originOutput = & git remote get-url origin 2>$null
        $originUrl = if ($originOutput) { $originOutput.Trim() } else { '' }
        $lastCommitOutput = & git log -1 --pretty=format:'%h %s' 2>$null
        $lastCommit = if ($lastCommitOutput) { $lastCommitOutput.Trim() } else { '' }
        $statusLines = @(& git status --short 2>$null)
        $trackedCount = @($statusLines | Where-Object { $_ -notmatch '^\?\?' }).Count
        $untrackedCount = @($statusLines | Where-Object { $_ -match '^\?\?' }).Count

        if ($repoRoot) { Write-InfoLine "仓库根目录: $repoRoot" }
        if ($realGitDir) { Write-InfoLine ".git 目录: $realGitDir" }
        if ($branch) {
            Write-InfoLine "当前分支: $branch"
        } else {
            Write-WarnLine "当前未处于正常分支，可能是 detached HEAD。"
        }
        if ($upstream) {
            Write-InfoLine "上游分支: $upstream"
        } else {
            Write-WarnLine "当前分支尚未配置上游分支。"
        }
        if ($originUrl) {
            Write-InfoLine "origin: $originUrl"
        } else {
            Write-WarnLine "当前仓库尚未配置 origin。"
        }
        if ($lastCommit) {
            Write-InfoLine "最近提交: $lastCommit"
        }
        Write-InfoLine "工作区状态: 已跟踪改动 $trackedCount 个，未跟踪文件 $untrackedCount 个"
    } else {
        Write-WarnLine "当前目录还不是 Git 仓库"
    }

    if ($UserName -or $Email) {
        Write-Step "设置 Git 账号"
        $args = @()
        if ($UserName) { $args += @('-UserName', $UserName) }
        if ($Email) { $args += @('-Email', $Email) }
        if ($GlobalAccount) { $args += '-Global' }
        & "$ScriptRoot\set-git-account.ps1" @args
    } else {
        Write-Step "检查 Git 账号"
        $localNameOutput = & git config --get user.name 2>$null
        $localEmailOutput = & git config --get user.email 2>$null
        $globalNameOutput = & git config --global --get user.name 2>$null
        $globalEmailOutput = & git config --global --get user.email 2>$null
        $localName = if ($localNameOutput) { $localNameOutput.Trim() } else { '' }
        $localEmail = if ($localEmailOutput) { $localEmailOutput.Trim() } else { '' }
        $globalName = if ($globalNameOutput) { $globalNameOutput.Trim() } else { '' }
        $globalEmail = if ($globalEmailOutput) { $globalEmailOutput.Trim() } else { '' }

        if ($localName -and $localEmail) {
            Write-Ok "本地账号: $localName <$localEmail>"
        } elseif ($globalName -and $globalEmail) {
            Write-Ok "全局账号: $globalName <$globalEmail>"
        } else {
            Write-WarnLine "未检测到 Git 用户名/邮箱。"
            Write-WarnLine '可执行: .\set-git-account.ps1'
        }
    }

    Write-Step "检查 GitHub CLI"
    $ghCmd = Get-Command gh -ErrorAction SilentlyContinue
    if ($ghCmd) {
        Write-Ok "gh 已安装: $((& gh --version | Select-Object -First 1).Trim())"
        try {
            $ghStatus = & gh auth status 2>&1
            if ($LASTEXITCODE -eq 0) {
                Write-Ok "gh 已登录"
            } else {
                Write-WarnLine "gh 已安装，但当前未登录。可执行: gh auth login"
            }
        } catch {
            Write-WarnLine "gh 状态检查失败。可手动执行: gh auth status"
        }
    } else {
        Write-WarnLine "未检测到 gh。可安装: winget install --id GitHub.cli"
    }

    Write-Step "初始化仓库"
    if (-not $isRepo) {
        $initChoice = Read-Host "是否将当前目录初始化为独立 Git 仓库？(Y/n)"
        if (-not $initChoice -or $initChoice -match '^(y|yes)$') {
            $initialBranch = Resolve-InitialBranchName

            & git init -b $initialBranch 1>$null 2>$null
            if ($LASTEXITCODE -ne 0) {
                & git init
                if ($LASTEXITCODE -ne 0) {
                    throw "git init 失败。"
                }

                & git branch -M $initialBranch 1>$null 2>$null
                if ($LASTEXITCODE -ne 0) {
                    throw "已初始化仓库，但设置默认分支 $initialBranch 失败。"
                }
            }

            Write-Ok "已初始化当前目录为 Git 仓库"
            Write-Ok "初始化分支: $initialBranch"
            $isRepo = $true
        } else {
            Write-WarnLine "已跳过仓库初始化。"
        }
    } else {
        Write-Ok "无需重复初始化"
    }

    Write-Step "配置远程仓库"
    if ($isRepo) {
        $existingRemotes = @(& git remote 2>$null)
        $originUrl = ''
        if ($existingRemotes -contains 'origin') {
            $originUrlOutput = & git remote get-url origin 2>$null
            $originUrl = if ($originUrlOutput) { $originUrlOutput.Trim() } else { '' }
        }
        if ($originUrl) {
            Write-Ok "origin: $originUrl"
            $resetRemote = Read-Host "是否重新配置 origin？(y/N)"
            if ($resetRemote -match '^(y|yes)$') {
                $remoteInfo = Resolve-RepositoryUrlInteractive
                & git remote set-url origin $remoteInfo.RemoteUrl
                if ($LASTEXITCODE -ne 0) {
                    throw "更新 origin 远程失败。"
                }
                Save-GitScriptProfile @remoteInfo
                Write-Ok "已更新 origin: $($remoteInfo.RemoteUrl)"
            }
        } else {
            $setRemote = Read-Host "当前仓库尚未配置 origin，是否现在配置？(Y/n)"
            if (-not $setRemote -or $setRemote -match '^(y|yes)$') {
                $remoteInfo = Resolve-RepositoryUrlInteractive
                & git remote add origin $remoteInfo.RemoteUrl
                if ($LASTEXITCODE -ne 0) {
                    throw "新增 origin 远程失败。"
                }
                Save-GitScriptProfile @remoteInfo
                Write-Ok "已新增 origin: $($remoteInfo.RemoteUrl)"
            } else {
                Write-WarnLine "已跳过 origin 配置。"
            }
        }
    }


    Write-Step "检查远程同步能力"
    $originUrlOutput = & git remote get-url origin 2>$null
    $originUrl = if ($originUrlOutput) { $originUrlOutput.Trim() } else { '' }
    if ($originUrl) {
        & git ls-remote origin 1>$null 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-Ok "已验证可访问远程仓库"
        } else {
            Write-WarnLine "无法访问远程仓库，请检查 SSH / HTTPS 认证"
        }
    } else {
        Write-WarnLine "当前没有 origin，暂时无法验证远程访问能力。"
    }

    Write-Step "常用下一步"
    Write-Host '  .\git-quick-status.ps1' -ForegroundColor DarkGray
    Write-Host '  .\pull-github.ps1' -ForegroundColor DarkGray
    Write-Host '  .\push-github.ps1' -ForegroundColor DarkGray
}
finally {
    Set-Location $OriginalLocation
}
