param(
    [string]$BranchName
)

$ErrorActionPreference = 'Stop'

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$OriginalLocation = Get-Location
$ProjectRoot = $ScriptRoot
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

function Get-RemoteBranches {
    param([string]$RemoteName)

    return @(
        & git ls-remote --heads $RemoteName 2>$null | ForEach-Object {
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

function Get-RemoteDefaultBranch {
    param([string]$RemoteName)

    $headLine = & git ls-remote --symref $RemoteName HEAD 2>$null | Select-Object -First 1
    if (-not $headLine) {
        return ''
    }

    if ($headLine -match 'refs/heads/([^\s]+)\s+HEAD') {
        return $Matches[1]
    }

    return ''
}

function Resolve-TargetBranch {
    param(
        [string[]]$RemoteBranches,
        [string]$CurrentBranch,
        [string]$DefaultBranch
    )

    if ($BranchName -and $BranchName.Trim()) {
        return $BranchName.Trim()
    }

    if (-not $RemoteBranches -or $RemoteBranches.Count -eq 0) {
        throw "未检测到远端分支。"
    }

    Write-Host "[clear-remote-branch] 远端可删除分支：" -ForegroundColor Cyan
    for ($i = 0; $i -lt $RemoteBranches.Count; $i++) {
        $branch = $RemoteBranches[$i]
        $marks = @()
        if ($branch -eq $CurrentBranch) { $marks += '当前本地分支' }
        if ($branch -eq $DefaultBranch) { $marks += '远端默认分支' }
        $suffix = if ($marks.Count -gt 0) { ' (' + ($marks -join ' / ') + ')' } else { '' }
        Write-Host ("  {0}. {1}{2}" -f ($i + 1), $branch, $suffix) -ForegroundColor DarkGray
    }

    while ($true) {
        $inputValue = Read-Host "请输入要删除的远端分支编号或名称"
        if (-not $inputValue -or -not $inputValue.Trim()) {
            Write-Host "[clear-remote-branch] 分支名不能为空，请重新输入。" -ForegroundColor Yellow
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

        Write-Host "[clear-remote-branch] 未匹配到远端分支，请重新输入。" -ForegroundColor Yellow
    }
}

try {
    Set-Location $ProjectRoot

    $ProjectRoot = Find-GitRepoRoot -StartPath $ProjectRoot
    if (-not $ProjectRoot) {
        throw "未能定位 Git 仓库根目录。请在仓库内运行该脚本。"
    }

    Set-Location $ProjectRoot

    $remoteName = 'origin'
    $remoteUrlOutput = (& git remote get-url $remoteName 2>$null)
    $remoteUrl = if ($remoteUrlOutput) { $remoteUrlOutput.Trim() } else { '' }
    if (-not $remoteUrl) {
        throw "未检测到 origin 远程。"
    }

    $currentBranchOutput = (& git branch --show-current 2>$null)
    $currentBranch = if ($currentBranchOutput) { $currentBranchOutput.Trim() } else { '' }

    $remoteBranches = Get-RemoteBranches -RemoteName $remoteName
    $defaultBranch = Get-RemoteDefaultBranch -RemoteName $remoteName
    $branch = Resolve-TargetBranch -RemoteBranches $remoteBranches -CurrentBranch $currentBranch -DefaultBranch $defaultBranch

    if ($defaultBranch -and $branch -eq $defaultBranch) {
        Write-Host "[clear-remote-branch] 你选择的是远端默认分支：$branch" -ForegroundColor Yellow
        Write-Host "[clear-remote-branch] 如果该仓库仍将它设为默认分支，GitHub 可能拒绝删除。" -ForegroundColor Yellow
    }

    Write-Host "[clear-remote-branch] 当前本地分支: $currentBranch" -ForegroundColor Yellow
    Write-Host "[clear-remote-branch] 目标远端分支: $branch" -ForegroundColor Yellow
    Write-Host "[clear-remote-branch] 远端: $remoteUrl" -ForegroundColor Yellow

    $confirm = Read-Host "请输入要删除的分支名以确认操作（$branch）"
    if ($confirm -ne $branch) {
        Write-Host "[clear-remote-branch] 已取消操作。" -ForegroundColor Yellow
        exit 0
    }

    Write-Host "[clear-remote-branch] 删除远端分支..." -ForegroundColor Cyan
    & git push $remoteName --delete $branch
    if ($LASTEXITCODE -ne 0) {
        throw "删除远端分支失败。若该分支是默认分支，请先在 GitHub 上切换默认分支后再试。"
    }

    Write-Host "[clear-remote-branch] 已删除远端分支：$branch" -ForegroundColor Green
}
finally {
    Set-Location $OriginalLocation
}
