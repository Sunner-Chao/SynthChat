param(
    [switch]$ShowOnly
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

Write-Host "Git 配置概览" -ForegroundColor Green
Write-Host ""

$currentBranch = (& git branch --show-current).Trim()
$originUrl = (& git remote get-url origin 2>$null).Trim()
$localName = (& git config --get user.name 2>$null).Trim()
$localEmail = (& git config --get user.email 2>$null).Trim()
$globalName = (& git config --global --get user.name 2>$null).Trim()
$globalEmail = (& git config --global --get user.email 2>$null).Trim()

Write-Host "当前分支: $currentBranch" -ForegroundColor Yellow
Write-Host "origin:   $originUrl" -ForegroundColor Yellow
Write-Host ""
Write-Host "本地仓库账号:" -ForegroundColor Cyan
Write-Host "  user.name  = $localName"
Write-Host "  user.email = $localEmail"
Write-Host ""
Write-Host "全局 Git 账号:" -ForegroundColor Cyan
Write-Host "  user.name  = $globalName"
Write-Host "  user.email = $globalEmail"

if (-not $ShowOnly) {
    Write-Host ""
    Write-Host "常用命令示例:" -ForegroundColor DarkGray
    Write-Host '.\set-git-account.ps1 -UserName "Your Name" -Email "you@example.com"' -ForegroundColor DarkGray
    Write-Host '.\set-git-account.ps1 -UserName "Your Name" -Email "you@example.com" -Global' -ForegroundColor DarkGray
    Write-Host '.\set-git-remote.ps1 -Repository "owner/repo" -Protocol ssh' -ForegroundColor DarkGray
    Write-Host '.\set-git-remote.ps1 -Repository "owner/repo" -Protocol https' -ForegroundColor DarkGray
}
