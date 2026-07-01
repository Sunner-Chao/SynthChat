param(
  [string]$UpdateManifestUrl = $env:SYNTHCHAT_UPDATE_MANIFEST_URL,
  [ValidateSet("all", "nsis", "msi")]
  [string]$Bundle = "nsis",
  [ValidateSet("offlineInstaller", "embedBootstrapper", "downloadBootstrapper", "skip", "config")]
  [string]$WebviewInstallMode = "offlineInstaller",
  [switch]$PreflightOnly,
  [switch]$SkipNpmInstall,
  [switch]$FastIncremental,
  [switch]$StrictBundlerExitCode,
  [switch]$RetryWithDownloadBootstrapper,
  [switch]$OpenOutput
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $RepoRoot

$NativeBuildScript = Join-Path $RepoRoot "scripts\build-windows-native.ps1"
$BundleRoot = Join-Path $RepoRoot "src-tauri\target\release\bundle"
$OutputRoot = Join-Path $RepoRoot "release-dist"

function Write-Step {
  param([Parameter(Mandatory = $true)][string]$Message)
  Write-Host ""
  Write-Host "==> $Message" -ForegroundColor Cyan
}

function Assert-Command {
  param([Parameter(Mandatory = $true)][string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command not found: $Name"
  }
}

function Get-GitHubManifestUrlFromRemote {
  $remote = ""
  try {
    $remote = (& git remote get-url origin 2>$null).Trim()
  } catch {
    return ""
  }
  if ([string]::IsNullOrWhiteSpace($remote)) {
    return ""
  }

  $owner = ""
  $repo = ""
  if ($remote -match "^git@github\.com:(?<owner>[^/]+)/(?<repo>[^/.]+)(?:\.git)?$") {
    $owner = $Matches.owner
    $repo = $Matches.repo
  } elseif ($remote -match "^https://github\.com/(?<owner>[^/]+)/(?<repo>[^/.]+)(?:\.git)?$") {
    $owner = $Matches.owner
    $repo = $Matches.repo
  }

  if ([string]::IsNullOrWhiteSpace($owner) -or [string]::IsNullOrWhiteSpace($repo)) {
    return ""
  }
  return "https://github.com/$owner/$repo/releases/latest/download/update-manifest.json"
}

function Copy-BundleArtifacts {
  if (-not (Test-Path -LiteralPath $BundleRoot)) {
    throw "Bundle output not found: $BundleRoot"
  }
  if (Test-Path -LiteralPath $OutputRoot) {
    Remove-Item -LiteralPath $OutputRoot -Recurse -Force
  }
  New-Item -ItemType Directory -Path $OutputRoot | Out-Null

  $patterns = @("*.exe", "*.msi", "*.msix", "*.zip", "*.json", "*.sig")
  $copied = @()
  foreach ($pattern in $patterns) {
    $items = Get-ChildItem -LiteralPath $BundleRoot -Recurse -File -Filter $pattern -ErrorAction SilentlyContinue
    foreach ($item in $items) {
      $target = Join-Path $OutputRoot $item.Name
      Copy-Item -LiteralPath $item.FullName -Destination $target -Force
      $copied += $target
    }
  }

  if ($copied.Count -eq 0) {
    throw "No installer artifacts were found under $BundleRoot"
  }

  Write-Host ""
  Write-Host "Copied artifacts:" -ForegroundColor Green
  foreach ($path in $copied) {
    Write-Host "  $path"
  }

  Write-UpdateManifest -CopiedArtifacts $copied
}

function Get-ReleaseAssetBaseUrl {
  $remote = ""
  try {
    $remote = (& git remote get-url origin 2>$null).Trim()
  } catch {
    return ""
  }
  if ([string]::IsNullOrWhiteSpace($remote)) {
    return ""
  }
  $owner = ""
  $repo = ""
  if ($remote -match "^git@github\.com:(?<owner>[^/]+)/(?<repo>[^/.]+)(?:\.git)?$") {
    $owner = $Matches.owner
    $repo = $Matches.repo
  } elseif ($remote -match "^https://github\.com/(?<owner>[^/]+)/(?<repo>[^/.]+)(?:\.git)?$") {
    $owner = $Matches.owner
    $repo = $Matches.repo
  }
  if ([string]::IsNullOrWhiteSpace($owner) -or [string]::IsNullOrWhiteSpace($repo)) {
    return ""
  }
  return "https://github.com/$owner/$repo/releases/latest/download"
}

function Get-AppVersion {
  $config = Get-Content -LiteralPath "src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json
  $version = [string]$config.version
  if ([string]::IsNullOrWhiteSpace($version)) {
    $version = [string](Get-Content -LiteralPath "package.json" -Raw | ConvertFrom-Json).version
  }
  return $version.Trim()
}

function Write-UpdateManifest {
  param([Parameter(Mandatory = $true)][string[]]$CopiedArtifacts)

  $installer = $CopiedArtifacts |
    Where-Object { $_ -match '\.(exe|msi|msix)$' } |
    Sort-Object {
      if ($_ -match '\.exe$') { 0 } elseif ($_ -match '\.msi$') { 1 } else { 2 }
    }, { Split-Path -Leaf $_ } |
    Select-Object -First 1

  if ([string]::IsNullOrWhiteSpace($installer)) {
    Write-Host "Skipped update-manifest.json: no native installer artifact found." -ForegroundColor Yellow
    return
  }

  $assetBaseUrl = Get-ReleaseAssetBaseUrl
  if ([string]::IsNullOrWhiteSpace($assetBaseUrl)) {
    Write-Host "Skipped update-manifest.json: GitHub origin remote not detected." -ForegroundColor Yellow
    return
  }

  $fileName = Split-Path -Leaf $installer
  $encodedFileName = [System.Uri]::EscapeDataString($fileName)
  $version = Get-AppVersion
  $manifest = [ordered]@{
    latestVersion = $version
    downloadUrl = "$assetBaseUrl/$encodedFileName"
    releaseUrl = $assetBaseUrl -replace "/download$", ""
    publishedAt = (Get-Date).ToUniversalTime().ToString("o")
    notes = "SynthChat $version"
  }
  $manifestPath = Join-Path $OutputRoot "update-manifest.json"
  $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8
  Write-Host "  $manifestPath"
}

Write-Step "Checking local toolchain"
Assert-Command "node"
Assert-Command "npm"
Assert-Command "cargo"
Assert-Command "rustc"
Assert-Command "git"

if (-not (Test-Path -LiteralPath $NativeBuildScript)) {
  throw "Native build script is missing: $NativeBuildScript"
}
if (-not (Test-Path -LiteralPath "src-tauri\src\main.rs")) {
  throw "Tauri entry is missing: src-tauri\src\main.rs"
}
$mainRs = Get-Content -LiteralPath "src-tauri\src\main.rs" -Raw
if ($mainRs -notmatch 'windows_subsystem\s*=\s*"windows"') {
  throw "Release no-console mode is missing in src-tauri\src\main.rs"
}

$ExternalChatTtsDir = "E:\SynthChat\ChatTTS"
if (Test-Path -LiteralPath $ExternalChatTtsDir) {
  Write-Host "ChatTTS external model/runtime detected: $ExternalChatTtsDir" -ForegroundColor Yellow
  Write-Host "Lightweight package mode keeps ChatTTS models, Python, torch, torchaudio, numpy, and ffmpeg outside the installer."
}

if ([string]::IsNullOrWhiteSpace($UpdateManifestUrl)) {
  $UpdateManifestUrl = Get-GitHubManifestUrlFromRemote
}
if (-not [string]::IsNullOrWhiteSpace($UpdateManifestUrl)) {
  $env:SYNTHCHAT_UPDATE_MANIFEST_URL = $UpdateManifestUrl.Trim()
  Write-Host "Update manifest: $env:SYNTHCHAT_UPDATE_MANIFEST_URL"
} else {
  Write-Host "Update manifest: not injected. You can set SYNTHCHAT_UPDATE_MANIFEST_URL or pass -UpdateManifestUrl." -ForegroundColor Yellow
}

if (-not $SkipNpmInstall) {
  if (-not (Test-Path -LiteralPath "node_modules")) {
    Write-Step "Installing frontend dependencies"
    if (Test-Path -LiteralPath "package-lock.json") {
      & npm ci
    } else {
      & npm install
    }
    if ($LASTEXITCODE -ne 0) {
      throw "npm dependency install failed with exit code $LASTEXITCODE"
    }
  }
}

$buildArgs = @(
  "-NoProfile",
  "-ExecutionPolicy", "Bypass",
  "-File", $NativeBuildScript,
  "-Bundle", $Bundle,
  "-WebviewInstallMode", $WebviewInstallMode
)
if (-not [string]::IsNullOrWhiteSpace($UpdateManifestUrl)) {
  $buildArgs += @("-UpdateManifestUrl", $UpdateManifestUrl.Trim())
}
if ($PreflightOnly) {
  $buildArgs += "-PreflightOnly"
}
if ($FastIncremental) {
  $buildArgs += "-FastIncremental"
}
if (-not $StrictBundlerExitCode) {
  $buildArgs += "-AcceptExistingArtifactOnTimeout"
}

Write-Step "Running native Windows build"
& powershell @buildArgs
if ($LASTEXITCODE -ne 0) {
  if ($RetryWithDownloadBootstrapper -and $WebviewInstallMode -ne "downloadBootstrapper") {
    Write-Host "Native build failed. Retrying with WebView2 downloadBootstrapper mode..." -ForegroundColor Yellow
    $retryArgs = @(
      "-NoProfile",
      "-ExecutionPolicy", "Bypass",
      "-File", $NativeBuildScript,
      "-Bundle", $Bundle,
      "-WebviewInstallMode", "downloadBootstrapper"
    )
    if (-not [string]::IsNullOrWhiteSpace($UpdateManifestUrl)) {
      $retryArgs += @("-UpdateManifestUrl", $UpdateManifestUrl.Trim())
    }
    if (-not $StrictBundlerExitCode) {
      $retryArgs += "-AcceptExistingArtifactOnTimeout"
    }
    if ($FastIncremental) {
      $retryArgs += "-FastIncremental"
    }
    & powershell @retryArgs
    if ($LASTEXITCODE -ne 0) {
      throw "Native Windows build failed with exit code $LASTEXITCODE, including downloadBootstrapper retry"
    }
    $WebviewInstallMode = "downloadBootstrapper"
  } else {
    throw "Native Windows build failed with exit code $LASTEXITCODE"
  }
}

if (-not $PreflightOnly) {
  Write-Step "Collecting installer artifacts"
  Copy-BundleArtifacts
  if ($OpenOutput) {
    Invoke-Item $OutputRoot
  }
}

Write-Host ""
Write-Host "Done." -ForegroundColor Green
Write-Host "Fresh Windows mode: WebView2 $WebviewInstallMode, silent installer config, bundled static resources."
Write-Host "Output: $OutputRoot"
