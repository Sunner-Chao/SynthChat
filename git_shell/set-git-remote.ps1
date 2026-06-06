param(
    [string]$Repository,

    [string]$RemoteName = 'origin',

    [ValidateSet('ssh', 'https')]
    [string]$Protocol = 'ssh',

    [string]$SshHost = 'github.com'
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

# 加载共享模块
$helperModule = Join-Path $ProjectRoot 'git-remote-helper.ps1'
. $helperModule

if (-not $Repository) {
    $Repository = Read-Host "Repository (owner/repo)"
}

if (-not $Repository) {
    throw "Repository 不能为空。"
}

if ($Repository -notmatch '^[^/]+/[^/]+$') {
    throw "Repository 格式必须是 owner/repo，例如 TheRealPiper/LStwinHR"
}

# 获取 SSH Host（仅在未显式传入时询问用户）
$SshHost = Get-SshHostFromParams -SshHost $SshHost -PSBoundParameters $PSBoundParameters

# 构造远程 URL
$remoteUrl = New-GitRemoteUrl -Repository $Repository -Protocol $Protocol -SshHost $SshHost

# 配置远程
Ensure-GitRemote -RemoteName $RemoteName -RemoteUrl $remoteUrl

# 显示配置结果
Show-GitRemoteConfig
