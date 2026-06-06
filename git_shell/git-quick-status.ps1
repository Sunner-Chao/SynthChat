param(
    [int]$RecentCommits = 5
)

$ErrorActionPreference = 'Stop'

$OriginalLocation = Get-Location
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = $ScriptRoot
if ((Split-Path -Leaf $ScriptRoot) -ieq 'git_shell') {
    $ProjectRoot = Split-Path -Parent $ScriptRoot
}

try {
    Set-Location $ProjectRoot

    $branchOutput = (& git branch --show-current 2>$null)
    $remoteOutput = (& git remote get-url origin 2>$null)
    $branch = if ($branchOutput) { $branchOutput.Trim() } else { '' }
    $remote = if ($remoteOutput) { $remoteOutput.Trim() } else { '' }

    Write-Host "Git 仓库状态" -ForegroundColor Green
    Write-Host "当前分支: $branch" -ForegroundColor Yellow
    Write-Host "远程仓库: $remote" -ForegroundColor Yellow
    Write-Host ""

    Write-Host "工作区状态" -ForegroundColor Cyan
    & git status --short

    Write-Host ""
    Write-Host "最近提交" -ForegroundColor Cyan
    & git log --oneline -n $RecentCommits
}
finally {
    Set-Location $OriginalLocation
}
