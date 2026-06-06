param(
    [string]$UserName,
    [string]$Email,
    [switch]$Global
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

$globalMode = $Global

if (-not $UserName) {
    $UserName = Read-Host "请输入 Git 用户名"
}

if (-not $UserName -or -not $UserName.Trim()) {
    throw "Git 用户名不能为空。"
}

if (-not $Email) {
    $Email = Read-Host "请输入 Git 邮箱"
}

if (-not $Email -or -not $Email.Trim()) {
    throw "Git 邮箱不能为空。"
}

if (-not $PSBoundParameters.ContainsKey('Global')) {
    $useGlobal = Read-Host "是否写入全局配置？(y/N)"
    if ($useGlobal -match '^(y|yes)$') {
        $globalMode = $true
    }
}

$scopeArgs = @()
if ($globalMode) {
    $scopeArgs += '--global'
}

Write-Host "[set-git-account] 设置 Git 用户名: $UserName" -ForegroundColor Cyan
& git config @scopeArgs user.name $UserName
if ($LASTEXITCODE -ne 0) {
    throw "设置 git user.name 失败"
}

Write-Host "[set-git-account] 设置 Git 邮箱: $Email" -ForegroundColor Cyan
& git config @scopeArgs user.email $Email
if ($LASTEXITCODE -ne 0) {
    throw "设置 git user.email 失败"
}

Write-Host "[set-git-account] 当前配置如下：" -ForegroundColor Green
& git config @scopeArgs --get user.name
& git config @scopeArgs --get user.email
