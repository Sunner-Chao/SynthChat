param(
    [string]$Message,
    [switch]$NoPush,
    [switch]$KeepTemp
)

$ErrorActionPreference = 'Stop'

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$OriginalLocation = Get-Location
$ProjectRoot = $ScriptRoot
$tempRepo = ''
if ((Split-Path -Leaf $ScriptRoot) -ieq 'git_shell') {
    $ProjectRoot = Split-Path -Parent $ScriptRoot
}

function Find-GitRepoRoot {
    param([string]$StartPath)

    $current = Resolve-Path $StartPath
    while ($current) {
        $gitDir = Join-Path $current.Path '.git'
        if (Test-Path $gitDir) {
            return $current.Path
        }

        $parent = Split-Path -Parent $current.Path
        if (-not $parent -or $parent -eq $current.Path) {
            break
        }

        $current = Resolve-Path $parent
    }

    return ''
}

function Resolve-CommitMessage {
    if ($Message -and $Message.Trim()) {
        return $Message.Trim()
    }

    $inputMessage = Read-Host "请输入 commit 信息（必填）"
    if (-not $inputMessage -or -not $inputMessage.Trim()) {
        throw "commit 信息不能为空。"
    }

    return $inputMessage.Trim()
}

function New-TempRepoPath {
    $base = Join-Path $env:TEMP ("lstwinhr-clear-" + [guid]::NewGuid().ToString("N"))
    return $base
}

function Get-TrackedFiles {
    return @(& git ls-files 2>$null)
}

function Get-RemoteBranches {
    param([string]$RemoteUrl)

    return @(
        & git ls-remote --heads $RemoteUrl 2>$null | ForEach-Object {
            $line = $_.Trim()
            if (-not $line) { return }
            $parts = $line -split '\s+'
            if ($parts.Length -lt 2) { return }
            $ref = $parts[1]
            if ($ref -match '^refs/heads/(.+)$') {
                $Matches[1]
            }
        } | Sort-Object -Unique
    )
}

function Resolve-TargetBranch {
    param(
        [string[]]$RemoteBranches,
        [string]$CurrentBranch
    )

    if (-not $RemoteBranches -or $RemoteBranches.Count -eq 0) {
        throw "未检测到远端分支。"
    }

    Write-Host "[clear-remote-repo] 远端可选分支：" -ForegroundColor Cyan
    for ($i = 0; $i -lt $RemoteBranches.Count; $i++) {
        $branchName = $RemoteBranches[$i]
        $mark = if ($branchName -eq $CurrentBranch) { ' (当前本地分支)' } else { '' }
        Write-Host ("  {0}. {1}{2}" -f ($i + 1), $branchName, $mark) -ForegroundColor DarkGray
    }

    while ($true) {
        $inputValue = Read-Host "请输入要清空的远端分支编号或名称（直接回车默认 $CurrentBranch）"
        if (-not $inputValue -or -not $inputValue.Trim()) {
            if ($RemoteBranches -contains $CurrentBranch) {
                return $CurrentBranch
            }

            Write-Host "[clear-remote-repo] 当前本地分支不在远端列表中，请手动输入分支编号或名称。" -ForegroundColor Yellow
            continue
        }

        $normalized = $inputValue.Trim()
        $index = 0
        if ([int]::TryParse($normalized, [ref]$index)) {
            if ($index -ge 1 -and $index -le $RemoteBranches.Count) {
                return $RemoteBranches[$index - 1]
            }
        }

        if ($RemoteBranches -contains $normalized) {
            return $normalized
        }

        Write-Host "[clear-remote-repo] 未匹配到远端分支，请重新输入。" -ForegroundColor Yellow
    }
}

try {
    Set-Location $ProjectRoot

    $ProjectRoot = Find-GitRepoRoot -StartPath $ProjectRoot
    if (-not $ProjectRoot) {
        throw "未能定位 Git 仓库根目录。请在仓库内运行该脚本。"
    }

    Set-Location $ProjectRoot

    $currentBranchOutput = (& git branch --show-current 2>$null)
    $currentBranch = if ($currentBranchOutput) { $currentBranchOutput.Trim() } else { '' }
    if (-not $currentBranch) {
        throw "未检测到当前分支，无法继续。"
    }

    $remoteUrlOutput = (& git remote get-url origin 2>$null)
    $remoteUrl = if ($remoteUrlOutput) { $remoteUrlOutput.Trim() } else { '' }
    if (-not $remoteUrl) {
        throw "未检测到 origin 远程。"
    }

    $remoteBranches = Get-RemoteBranches -RemoteUrl $remoteUrl
    $branch = Resolve-TargetBranch -RemoteBranches $remoteBranches -CurrentBranch $currentBranch

    Write-Host "[clear-remote-repo] 当前本地分支: $currentBranch" -ForegroundColor Yellow
    Write-Host "[clear-remote-repo] 目标远端分支: $branch" -ForegroundColor Yellow
    Write-Host "[clear-remote-repo] 远端: $remoteUrl" -ForegroundColor Yellow
    $confirm = Read-Host "确认执行清空仓库内容吗？(y/N)"
    if ($confirm -notmatch '^(y|yes)$') {
        Write-Host "[clear-remote-repo] 已取消操作。" -ForegroundColor Yellow
        exit 0
    }

    $commitMessage = Resolve-CommitMessage

    $tempRepo = New-TempRepoPath
    Write-Host "[clear-remote-repo] 创建临时仓库副本..." -ForegroundColor Cyan
    Write-Host "[clear-remote-repo] 临时目录: $tempRepo" -ForegroundColor DarkGray
    & git clone --depth 1 --branch $branch --single-branch $remoteUrl $tempRepo
    if ($LASTEXITCODE -ne 0) {
        throw "git clone 临时仓库失败。"
    }

    Set-Location $tempRepo

    Write-Host "[clear-remote-repo] 在临时仓库中删除所有已跟踪文件..." -ForegroundColor Cyan
    $trackedFiles = Get-TrackedFiles
    if (-not $trackedFiles -or $trackedFiles.Count -eq 0) {
        Write-Host "[clear-remote-repo] 当前远端分支已经没有已跟踪文件，无需再清空。" -ForegroundColor Yellow
        exit 0
    }

    & git rm -r -f -- .
    if ($LASTEXITCODE -ne 0) {
        throw "git rm 失败。"
    }

    Write-Host "[clear-remote-repo] 提交信息: $commitMessage" -ForegroundColor Yellow
    & git commit -m $commitMessage
    if ($LASTEXITCODE -ne 0) {
        throw "git commit 失败。"
    }

    if ($NoPush) {
        Write-Host "[clear-remote-repo] 已在临时仓库完成提交，未推送到远端（NoPush）。" -ForegroundColor Green
        if (-not $KeepTemp) {
            Write-Host "[clear-remote-repo] 由于使用了 NoPush，已自动保留临时目录供检查。" -ForegroundColor Yellow
            $KeepTemp = $true
        }
        exit 0
    }

    Write-Host "[clear-remote-repo] 推送到远端..." -ForegroundColor Cyan
    Write-Host "[clear-remote-repo] 使用 --force-with-lease 覆盖远端目标分支..." -ForegroundColor DarkGray
    & git push --force-with-lease origin $branch
    if ($LASTEXITCODE -ne 0) {
        throw "git push 失败。"
    }

    Write-Host "[clear-remote-repo] 已完成远端当前分支内容清空。" -ForegroundColor Green
}
finally {
    Set-Location $OriginalLocation

    if ($tempRepo -and (Test-Path $tempRepo) -and -not $KeepTemp) {
        Remove-Item $tempRepo -Recurse -Force
    }
}
