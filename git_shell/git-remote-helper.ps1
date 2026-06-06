# git-remote-helper.ps1 - Git 远程配置共享模块
# 提供 SSH Host 别名询问、URL 解析、远程设置等公共逻辑

$ErrorActionPreference = 'Stop'

# ===== 辅助函数 =====

function Write-Step {
    param([string]$Title, [string]$Color = 'Cyan')
    Write-Host ""
    Write-Host "== $Title ==" -ForegroundColor $Color
}

function Write-Info {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor DarkGray
}

function Write-Menu {
    param([string]$Title, [string[]]$Options, [int]$Default = 1)
    Write-Host ""
    Write-Host "$Title" -ForegroundColor Yellow
    for ($i = 0; $i -lt $Options.Count; $i++) {
        $mark = if ($i + 1 -eq $Default) { " [默认]" } else { "" }
        Write-Host "  $($i + 1). $($Options[$i])$mark" -ForegroundColor White
    }
    Write-Host ""
}

function Read-MenuChoice {
    param([string]$Prompt, [int]$Max, [int]$Default)
    while ($true) {
        $input = Read-Host "$Prompt (直接回车选择 $Default)"
        if ([string]::IsNullOrWhiteSpace($input)) {
            return $Default
        }
        $num = 0
        if ([int]::TryParse($input, [ref]$num) -and $num -ge 1 -and $num -le $Max) {
            return $num
        }
        Write-Host "  输入无效，请输入 1-$Max" -ForegroundColor Red
    }
}

# ===== SSH Host 询问 =====

function Get-SshHost {
    <#
    .SYNOPSIS
        询问 SSH Host 别名，优先使用默认值
    .PARAMETER DefaultHost
        默认 SSH Host，默认为 github.com
    .PARAMETER DefaultAlias
        默认别名，默认为 github-sunner
    .OUTPUTS
        最终使用的 SSH Host 字符串
    #>
    param(
        [string]$DefaultHost = 'github.com',
        [string]$DefaultAlias = 'github-sunner'
    )

    $useAlias = Read-Host "是否使用 SSH Host 别名？(y/N)"
    if ($useAlias -match '^(y|yes)$') {
        $aliasInput = Read-Host "请输入 SSH Host 别名（直接回车默认 $DefaultAlias）"
        if (-not [string]::IsNullOrWhiteSpace($aliasInput)) {
            return $aliasInput.Trim()
        }
        return $DefaultAlias
    }
    return $DefaultHost
}

function Get-SshHostFromParams {
    <#
    .SYNOPSIS
        从脚本参数中获取 SSH Host，仅在未显式传入时询问用户
    .PARAMETER SshHost
        脚本传入的 SshHost 参数值（可能是默认值）
    .PARAMETER PSBoundParameters
        脚本的 $PSBoundParameters，用于判断是否显式传入
    .PARAMETER DefaultAlias
        默认别名，默认为 github-sunner
    #>
    param(
        [string]$SshHost = 'github.com',
        [hashtable]$PSBoundParameters,
        [string]$DefaultAlias = 'github-sunner'
    )

    if ($PSBoundParameters.ContainsKey('SshHost')) {
        return $SshHost
    }
    return Get-SshHost -DefaultAlias $DefaultAlias
}

# ===== URL 解析 =====

function Resolve-GitRemoteUrl {
    <#
    .SYNOPSIS
        解析 Git 远程 URL，支持 SSH Host 别名替换
    .PARAMETER SshUrl
        SSH 格式的 URL（如 git@github.com:owner/repo.git）
    .PARAMETER HttpsUrl
        HTTPS 格式的 URL（如 https://github.com/owner/repo.git）
    .PARAMETER Protocol
        协议类型：ssh 或 https
    .PARAMETER SshHost
        SSH Host 别名（如 github-sunner）
    .OUTPUTS
        解析后的远程 URL 字符串
    #>
    param(
        [Parameter(Mandatory)]
        [string]$SshUrl,

        [Parameter(Mandatory)]
        [string]$HttpsUrl,

        [ValidateSet('ssh', 'https')]
        [string]$Protocol = 'ssh',

        [string]$SshHost = 'github.com'
    )

    if ($Protocol -eq 'ssh') {
        # 处理 SSH Host 别名替换：git@github.com:xxx -> git@custom-host:xxx
        if ($SshHost -and $SshHost -ne 'github.com' -and $SshUrl -match '^git@github\.com:(.+)$') {
            return "git@${SshHost}:$($Matches[1])"
        }
        return $SshUrl
    }
    return $HttpsUrl
}

function New-GitRemoteUrl {
    <#
    .SYNOPSIS
        根据仓库路径构造 Git 远程 URL
    .PARAMETER Repository
        仓库路径，格式为 owner/repo
    .PARAMETER Protocol
        协议类型：ssh 或 https
    .PARAMETER SshHost
        SSH Host 别名（如 github-sunner）
    .OUTPUTS
        构造的远程 URL 字符串
    #>
    param(
        [Parameter(Mandatory)]
        [string]$Repository,

        [ValidateSet('ssh', 'https')]
        [string]$Protocol = 'ssh',

        [string]$SshHost = 'github.com'
    )

    if ($Protocol -eq 'ssh') {
        return "git@${SshHost}:$Repository.git"
    }
    return "https://github.com/$Repository.git"
}

# ===== 远程配置 =====

function Get-ExistingRemoteUrl {
    <#
    .SYNOPSIS
        获取已存在的远程 URL
    .PARAMETER RemoteName
        远程名称（如 origin）
    .OUTPUTS
        远程 URL 字符串，若不存在则返回空字符串
    #>
    param(
        [string]$RemoteName = 'origin'
    )
    try {
        return (& git remote get-url $RemoteName 2>$null).Trim()
    } catch {
        return ''
    }
}

function Ensure-GitRemote {
    <#
    .SYNOPSIS
        确保远程仓库已配置，存在则更新，不存在则新增
    .PARAMETER RemoteName
        远程名称（如 origin）
    .PARAMETER RemoteUrl
        远程 URL
    .PARAMETER Silent
        静默模式，减少输出
    #>
    param(
        [string]$RemoteName = 'origin',
        [Parameter(Mandatory)]
        [string]$RemoteUrl,
        [switch]$Silent
    )

    $existingUrl = Get-ExistingRemoteUrl -RemoteName $RemoteName
    if (-not $Silent) {
        Write-Host "[git-remote-helper] 配置 Git 远程" -ForegroundColor Cyan
    }

    if ($existingUrl) {
        if (-not $Silent) {
            Write-Host "  更新远程 $RemoteName -> $RemoteUrl" -ForegroundColor Cyan
        }
        & git remote set-url $RemoteName $RemoteUrl
    } else {
        if (-not $Silent) {
            Write-Host "  新增远程 $RemoteName -> $RemoteUrl" -ForegroundColor Cyan
        }
        & git remote add $RemoteName $RemoteUrl
    }
    if ($LASTEXITCODE -ne 0) {
        throw "配置 git remote 失败。"
    }
}

function Show-GitRemoteConfig {
    <#
    .SYNOPSIS
        显示当前 Git 远程配置
    #>
    Write-Host "[git-remote-helper] 当前远程配置：" -ForegroundColor Green
    & git remote -v
}

function Remove-GitRemote {
    <#
    .SYNOPSIS
        移除指定的远程配置
    .PARAMETER RemoteName
        远程名称（如 origin）
    #>
    param(
        [string]$RemoteName = 'origin'
    )

    $existingRemote = & git remote 2>$null
    if ($existingRemote -notcontains $RemoteName) {
        Write-Host "[git-remote-helper] 远程 $RemoteName 不存在，无需移除" -ForegroundColor Yellow
        return
    }

    Write-Host "[git-remote-helper] 移除远程 $RemoteName" -ForegroundColor Cyan
    & git remote remove $RemoteName
    if ($LASTEXITCODE -ne 0) {
        throw "git remote remove 失败"
    }
}

