param(
    [string]$TagName,
    [switch]$KeepLocalTag
)

$ErrorActionPreference = 'Stop'

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$OriginalLocation = Get-Location
Set-Location $ScriptRoot

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

function Normalize-TagName {
    param([string]$InputTag)

    if (-not $InputTag) {
        return ''
    }

    $normalized = $InputTag.Trim()
    if (-not $normalized) {
        return ''
    }

    if ($normalized -notmatch '^v') {
        $normalized = "v$normalized"
    }

    return $normalized
}

function Resolve-TagName {
    if ($TagName) {
        return Normalize-TagName -InputTag $TagName
    }

    $remoteTags = @(& git ls-remote --tags origin 2>$null | ForEach-Object {
        $line = $_.Trim()
        if (-not $line) { return }
        $parts = $line -split '\s+'
        if ($parts.Length -lt 2) { return }
        $ref = $parts[1]
        if ($ref -match '^refs/tags/(.+?)(\^\{\})?$') { $Matches[1] }
    } | Sort-Object -Unique)

    $localTags = @(& git tag --list 'v*')

    if (-not $remoteTags -or $remoteTags.Count -eq 0) {
        if ($localTags -and $localTags.Count -gt 0) {
            Write-Host "[delete-remote-tag] 当前未检测到远端版本标签。" -ForegroundColor Yellow
            Write-Host "[delete-remote-tag] 当前本地标签：" -ForegroundColor Cyan
            Write-Host ("  " + ($localTags -join ', ')) -ForegroundColor DarkGray

            while ($true) {
                $inputTag = Read-Host "请输入要删除的本地版本标签（例如 v0.0.1）"
                $resolvedTag = Normalize-TagName -InputTag $inputTag
                if (-not $resolvedTag) {
                    Write-Host "[delete-remote-tag] 标签名不能为空，请重新输入。" -ForegroundColor Yellow
                    continue
                }

                if ($localTags -contains $resolvedTag) {
                    return @{
                        TagName      = $resolvedTag
                        RemoteExists = $false
                    }
                }

                Write-Host "[delete-remote-tag] 本地不存在标签 $resolvedTag，请重新输入。" -ForegroundColor Yellow
            }
        }

        throw "未检测到远端版本标签。"
    }

    Write-Host "[delete-remote-tag] 当前远端标签：" -ForegroundColor Cyan
    Write-Host ("  " + ($remoteTags -join ', ')) -ForegroundColor DarkGray

    while ($true) {
        $inputTag = Read-Host "请输入要删除的版本标签（例如 v0.0.1）"
        $resolvedTag = Normalize-TagName -InputTag $inputTag
        if (-not $resolvedTag) {
            Write-Host "[delete-remote-tag] 标签名不能为空，请重新输入。" -ForegroundColor Yellow
            continue
        }

        if ($remoteTags -contains $resolvedTag) {
            return @{
                TagName      = $resolvedTag
                RemoteExists = $true
            }
        }

        Write-Host "[delete-remote-tag] 远端不存在标签 $resolvedTag，请重新输入。" -ForegroundColor Yellow
    }
}

try {
    $ProjectRoot = Find-GitRepoRoot -StartPath $ScriptRoot
    if (-not $ProjectRoot) {
        throw "未能定位 Git 仓库根目录。请在仓库内运行该脚本。"
    }

    Set-Location $ProjectRoot

    $remoteUrl = (& git remote get-url origin 2>$null).Trim()
    if (-not $remoteUrl) {
        throw "未检测到 origin 远程。"
    }

    & git fetch --tags origin
    if ($LASTEXITCODE -ne 0) {
        throw "git fetch --tags 失败。"
    }

    $resolvedTag = Resolve-TagName
    $resolvedTagName = $resolvedTag.TagName
    $remoteExists = [bool]$resolvedTag.RemoteExists

    Write-Host "[delete-remote-tag] 远端: $remoteUrl" -ForegroundColor Yellow
    if ($remoteExists) {
        Write-Host "[delete-remote-tag] 即将删除远端标签: $resolvedTagName" -ForegroundColor Yellow
    } else {
        Write-Host "[delete-remote-tag] 即将删除本地标签: $resolvedTagName" -ForegroundColor Yellow
    }
    $confirm = Read-Host "确认删除该远端标签吗？(y/N)"
    if ($confirm -notmatch '^(y|yes)$') {
        Write-Host "[delete-remote-tag] 已取消操作。" -ForegroundColor Yellow
        exit 0
    }

    if ($remoteExists) {
        Write-Host "[delete-remote-tag] 删除远端标签..." -ForegroundColor Cyan
        & git push origin ":refs/tags/$resolvedTagName"
        if ($LASTEXITCODE -ne 0) {
            throw "删除远端标签失败。"
        }
    }

    if (-not $KeepLocalTag) {
        & git rev-parse "refs/tags/$resolvedTagName" 1>$null 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "[delete-remote-tag] 删除本地标签..." -ForegroundColor Cyan
            & git tag -d $resolvedTagName
            if ($LASTEXITCODE -ne 0) {
                throw "删除本地标签失败。"
            }
        }
    }

    Write-Host "[delete-remote-tag] 标签删除完成：$resolvedTagName" -ForegroundColor Green
}
finally {
    Set-Location $OriginalLocation
}
