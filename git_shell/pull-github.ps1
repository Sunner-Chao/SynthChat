param(
    [string]$RepositoryUrl,
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$OriginalLocation = Get-Location
$WorkRoot = $OriginalLocation.Path

. (Join-Path $ScriptRoot 'git-script-profile.ps1')
$ProfileDefaults = Get-GitScriptProfile

function Resolve-WorkRoot {
    $path = $WorkRoot
    if ((Split-Path -Leaf $path) -ieq 'git_shell') {
        return (Split-Path -Parent $path)
    }

    return $path
}

function Resolve-RepositoryUrl {
    $targetRoot = Resolve-WorkRoot
    $repoRoot = Get-RepoRoot -Path $targetRoot
    if ($repoRoot) {
        $currentOrigin = & git -C $repoRoot remote get-url origin 2>$null
        if ($currentOrigin) {
            return $currentOrigin.Trim()
        }
    }

    if ($RepositoryUrl) {
        return $RepositoryUrl
    }

    if ($ProfileDefaults.RemoteUrl) {
        return $ProfileDefaults.RemoteUrl
    }

    if ($ProfileDefaults.Repository) {
        if ($ProfileDefaults.Protocol -eq 'https') {
            return "https://github.com/$($ProfileDefaults.Repository).git"
        }

        return "git@$($ProfileDefaults.SshHost):$($ProfileDefaults.Repository).git"
    }

    $repository = Read-Host "未找到默认仓库配置，请输入 Repository (owner/repo)"
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

    $resolvedUrl = if ($protocol -eq 'https') {
        "https://github.com/$repository.git"
    } else {
        "git@${sshHost}:$repository.git"
    }

    Save-GitScriptProfile -Repository $repository -RemoteUrl $resolvedUrl -Protocol $protocol -SshHost $sshHost -RemoteName $ProfileDefaults.RemoteName
    $script:ProfileDefaults = Get-GitScriptProfile

    Write-Host "[pull-github] 已保存默认仓库配置，后续可直接复用。" -ForegroundColor Green
    return $resolvedUrl
}

function Resolve-RepoName {
    param([string]$Url)

    $name = [System.IO.Path]::GetFileName($Url)
    if (-not $name) {
        return 'repo'
    }

    if ($name.EndsWith('.git')) {
        return $name.Substring(0, $name.Length - 4)
    }

    return $name
}

function Resolve-PullMode {
    Write-Host "[pull-github] 请选择本次拉取模式：" -ForegroundColor Cyan
    Write-Host "  1. 全量拉取（覆盖本地，按远端为准）" -ForegroundColor DarkGray
    Write-Host "  2. 仅拉更新内容（默认，尽量保留本地改动）" -ForegroundColor DarkGray
    $choice = Read-Host "请输入 1 或 2（直接回车默认 2）"

    if ($choice -eq '1') {
        return 'full_override'
    }

    return 'update_only'
}

function Normalize-VersionRef {
    param([string]$InputVersion)

    if (-not $InputVersion) {
        return ''
    }

    return $InputVersion.Trim()
}

function Get-RemoteVersionTags {
    return @(
        & git ls-remote --tags origin 2>$null | ForEach-Object {
            $line = $_.Trim()
            if (-not $line) { return }
            $parts = $line -split '\s+'
            if ($parts.Length -lt 2) { return }
            $ref = $parts[1]
            if ($ref -match '^refs/tags/(.+?)(\^\{\})?$') {
                $Matches[1]
            }
        } | Sort-Object -Unique
    )
}

function Get-LatestRemoteTag {
    param([string[]]$Tags)

    if (-not $Tags -or $Tags.Count -eq 0) {
        return ''
    }

    $sorted = $Tags | ForEach-Object {
        $tag = $_
        $raw = $tag.TrimStart('v')
        $versionValue = try {
            [version]$raw
        } catch {
            [version]'0.0.0'
        }

        [PSCustomObject]@{
            Tag     = $tag
            Version = $versionValue
        }
    } | Sort-Object Version, Tag

    return $sorted[-1].Tag
}

function Resolve-VersionChoice {
    if ($Version) {
        $requested = Normalize-VersionRef -InputVersion $Version
    } else {
        $useVersion = Read-Host "是否切换到指定版本号/标签？(y/N)"
        if ($useVersion -notmatch '^(y|yes)$') {
            return ''
        }
        $requested = ''
    }

    $remoteTags = Get-RemoteVersionTags
    if ($remoteTags.Count -eq 0) {
        Write-Host "[pull-github] 当前远端没有可用版本标签，将继续使用默认分支。" -ForegroundColor Yellow
        return ''
    }

    $latestTag = Get-LatestRemoteTag -Tags $remoteTags
    Write-Host "[pull-github] 当前远端可用版本标签：" -ForegroundColor Cyan
    Write-Host ("  " + ($remoteTags -join ', ')) -ForegroundColor DarkGray
    Write-Host "[pull-github] 直接回车将默认使用最新版本：$latestTag" -ForegroundColor DarkGray

    while ($true) {
        $inputVersion = $requested
        if (-not $inputVersion) {
            $inputVersion = Read-Host "请输入要切换的版本号或标签（例如 1.0.0 / v1.0.0）"
        }

        if (-not $inputVersion -or -not $inputVersion.Trim()) {
            return $latestTag
        }

        $normalized = Normalize-VersionRef -InputVersion $inputVersion
        $candidateRefs = @($normalized)
        if ($normalized -notmatch '^v') {
            $candidateRefs += "v$normalized"
        }

        foreach ($candidate in $candidateRefs) {
            if ($remoteTags -contains $candidate) {
                return $candidate
            }
        }

        Write-Host "[pull-github] 远端不存在版本标签 $normalized，请从上面的列表中选择。" -ForegroundColor Yellow
        $requested = ''
    }
}

function Get-RepoRoot {
    param([string]$Path)

    $candidate = Resolve-Path $Path
    while ($candidate) {
        if (Test-Path (Join-Path $candidate.Path '.git')) {
            return $candidate.Path
        }

        $parent = Split-Path -Parent $candidate.Path
        if (-not $parent -or $parent -eq $candidate.Path) {
            break
        }

        $candidate = Resolve-Path $parent
    }

    return ''
}

function Ensure-RepositoryContext {
    $targetRoot = Resolve-WorkRoot
    $script:RepoRoot = Get-RepoRoot -Path $targetRoot
    if ($script:RepoRoot) {
        Set-Location $script:RepoRoot
        return
    }

    throw "当前目录还不是独立 Git 仓库：$targetRoot`n请先运行 .\init-git-env.ps1 初始化当前目录，然后再执行 .\pull-github.ps1。"
}

function Ensure-OriginUrl {
    param([string]$ResolvedRepositoryUrl)

    $currentOrigin = (& git remote get-url origin 2>$null)
    if (-not $currentOrigin) {
        & git remote add origin $ResolvedRepositoryUrl
        if ($LASTEXITCODE -ne 0) {
            throw "配置 origin 远程失败。"
        }
        return
    }

    if ($currentOrigin.Trim() -ne $ResolvedRepositoryUrl.Trim()) {
        & git remote set-url origin $ResolvedRepositoryUrl
        if ($LASTEXITCODE -ne 0) {
            throw "更新 origin 远程失败。"
        }
    }
}

function Get-StatusLines {
    return @(& git status --short 2>$null)
}

function Has-UnmergedFiles {
    $unmerged = @(& git diff --name-only --diff-filter=U 2>$null)
    return ($unmerged.Count -gt 0)
}

function Write-StatusSummary {
    param([string[]]$Lines)

    if (-not $Lines -or $Lines.Count -eq 0) {
        Write-Host "[pull-github] 当前工作区干净。" -ForegroundColor Green
        return
    }

    $tracked = @($Lines | Where-Object { $_ -notmatch '^\?\?' }).Count
    $untracked = @($Lines | Where-Object { $_ -match '^\?\?' }).Count
    Write-Host "[pull-github] 当前工作区存在改动：已跟踪改动 $tracked 个，未跟踪文件 $untracked 个。" -ForegroundColor Yellow
}

function Get-RemoteDefaultBranch {
    $headInfo = @(& git ls-remote --symref origin HEAD 2>$null)
    $headLine = $headInfo | Where-Object { $_ -match '^ref:\s+refs/heads/' } | Select-Object -First 1
    if (-not $headLine) {
        return ''
    }

    if ($headLine -match 'refs/heads/([^\s]+)\s+HEAD') {
        return $Matches[1]
    }

    return ''
}

function Ensure-LocalBranch {
    param([string]$Branch)

    $currentBranchOutput = & git branch --show-current 2>$null
    $currentBranch = if ($currentBranchOutput) { $currentBranchOutput.Trim() } else { '' }
    if ($currentBranch -eq $Branch) {
        return
    }

    & git show-ref --verify --quiet "refs/heads/$Branch"
    if ($LASTEXITCODE -eq 0) {
        & git checkout $Branch
    } else {
        & git checkout -b $Branch --track "origin/$Branch"
    }

    if ($LASTEXITCODE -ne 0) {
        throw "切换到分支 $Branch 失败。"
    }
}

function Invoke-FullBranchPull {
    param([string]$Branch)

    Write-Host "[pull-github] 全量拉取默认分支: $Branch" -ForegroundColor Cyan
    & git checkout -f -B $Branch "origin/$Branch"
    if ($LASTEXITCODE -ne 0) {
        throw "检出远端分支 $Branch 失败。"
    }

    & git reset --hard "origin/$Branch"
    if ($LASTEXITCODE -ne 0) {
        throw "git reset --hard 失败。"
    }

    & git clean -fd
    if ($LASTEXITCODE -ne 0) {
        throw "git clean -fd 失败。"
    }
}

function Invoke-UpdateBranchPull {
    param([string]$Branch)

    if (Has-UnmergedFiles) {
        throw "当前仓库存在未解决冲突文件。请先处理冲突，或改用全量拉取。"
    }

    Ensure-LocalBranch -Branch $Branch
    Write-StatusSummary -Lines (Get-StatusLines)
    Write-Host "[pull-github] 拉取默认分支最新更新..." -ForegroundColor Cyan
    & git pull --rebase --autostash origin $Branch
    if ($LASTEXITCODE -ne 0) {
        throw "git pull --rebase 失败。常见原因：本地冲突、远端变更复杂或当前工作区不干净。"
    }
}

function Invoke-FullVersionPull {
    param([string]$VersionRef)

    Write-Host "[pull-github] 全量切换到版本标签: $VersionRef" -ForegroundColor Cyan
    & git reset --hard HEAD 1>$null 2>$null
    & git clean -fd 1>$null 2>$null
    & git checkout -f $VersionRef
    if ($LASTEXITCODE -ne 0) {
        throw "切换到版本标签 $VersionRef 失败。"
    }
}

function Invoke-UpdateVersionPull {
    param([string]$VersionRef)

    if (Has-UnmergedFiles) {
        throw "当前仓库存在未解决冲突文件。请先处理冲突，或改用全量拉取。"
    }

    $statusLines = Get-StatusLines
    $trackedChanges = @($statusLines | Where-Object { $_ -notmatch '^\?\?' }).Count
    if ($trackedChanges -gt 0) {
        throw "当前工作区存在已跟踪改动，无法安全切换到指定版本 $VersionRef。请先提交/清理，或改用全量拉取。"
    }

    Write-StatusSummary -Lines $statusLines
    Write-Host "[pull-github] 切换到版本标签: $VersionRef" -ForegroundColor Cyan
    & git checkout $VersionRef
    if ($LASTEXITCODE -ne 0) {
        throw "切换到版本标签 $VersionRef 失败。"
    }
}


try {
    $resolvedRepositoryUrl = Resolve-RepositoryUrl
    Ensure-RepositoryContext
    Ensure-OriginUrl -ResolvedRepositoryUrl $resolvedRepositoryUrl

    Write-Host "[pull-github] 获取远端最新信息..." -ForegroundColor Cyan
    & git fetch --tags origin
    if ($LASTEXITCODE -ne 0) {
        throw "git fetch 失败。请先检查远端仓库地址、SSH 配置或网络。"
    }

    $pullMode = Resolve-PullMode
    $versionRef = Resolve-VersionChoice

    if ($versionRef) {
        if ($pullMode -eq 'full_override') {
            Invoke-FullVersionPull -VersionRef $versionRef
        } else {
            Invoke-UpdateVersionPull -VersionRef $versionRef
        }

        Write-Host "[pull-github] 已完成版本切换: $versionRef" -ForegroundColor Green

        exit 0
    }

    $defaultBranch = Get-RemoteDefaultBranch
    if (-not $defaultBranch) {
        throw "未能识别远端默认分支。"
    }

    if ($pullMode -eq 'full_override') {
        Invoke-FullBranchPull -Branch $defaultBranch
    } else {
        Invoke-UpdateBranchPull -Branch $defaultBranch
    }

    Write-Host "[pull-github] 已完成拉取。默认分支: $defaultBranch" -ForegroundColor Green
}
finally {
    Set-Location $OriginalLocation
}
