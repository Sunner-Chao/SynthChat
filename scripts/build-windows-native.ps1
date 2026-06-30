param(
  [string]$UpdateManifestUrl = $env:SYNTHCHAT_UPDATE_MANIFEST_URL,
  [ValidateSet("all", "nsis", "msi")]
  [string]$Bundle = "all",
  [ValidateSet("config", "offlineInstaller", "embedBootstrapper", "downloadBootstrapper", "skip")]
  [string]$WebviewInstallMode = "config",
  [switch]$SkipPreflight,
  [switch]$PreflightOnly,
  [switch]$FastIncremental,
  [switch]$AcceptExistingArtifactOnTimeout
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $RepoRoot
$TauriConfigPath = "src-tauri\tauri.conf.json"
$OriginalTauriConfig = $null
$BuildStartedAt = Get-Date
$FastConfigPath = $null

function Assert-RequiredPath {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [Parameter(Mandatory = $true)]
    [string]$Label
  )
  if (-not (Test-Path -LiteralPath $Path)) {
    throw "$Label is missing: $Path"
  }
}

function Write-Utf8NoBom {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [Parameter(Mandatory = $true)]
    [string]$Value
  )
  $encoding = New-Object System.Text.UTF8Encoding $false
  $parent = Split-Path -Parent $Path
  if (-not [string]::IsNullOrWhiteSpace($parent) -and -not (Test-Path -LiteralPath $parent)) {
    New-Item -ItemType Directory -Path $parent | Out-Null
  }
  $fullPath = if (Test-Path -LiteralPath $Path) {
    (Resolve-Path -LiteralPath $Path).Path
  } else {
    [System.IO.Path]::GetFullPath($Path)
  }
  [System.IO.File]::WriteAllText($fullPath, $Value, $encoding)
}

function Get-LatestBundleArtifact {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Bundle
  )
  $bundleRoot = Join-Path $RepoRoot "src-tauri\target\release\bundle"
  $searchRoots = @()
  if ($Bundle -eq "all") {
    $searchRoots = @(
      (Join-Path $bundleRoot "nsis"),
      (Join-Path $bundleRoot "msi")
    )
  } else {
    $searchRoots = @((Join-Path $bundleRoot $Bundle))
  }
  $extensions = @("*.exe", "*.msi", "*.msix")
  $items = @()
  foreach ($root in $searchRoots) {
    if (-not (Test-Path -LiteralPath $root)) { continue }
    foreach ($extension in $extensions) {
      $items += Get-ChildItem -LiteralPath $root -Filter $extension -File -ErrorAction SilentlyContinue
    }
  }
  $items | Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

function Get-LatestWriteTimeUtc {
  param([Parameter(Mandatory = $true)][string[]]$Paths)
  $latest = [DateTime]::MinValue
  foreach ($path in $Paths) {
    if (-not (Test-Path -LiteralPath $path)) { continue }
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer) {
      $children = Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue
      foreach ($child in $children) {
        if ($child.LastWriteTimeUtc -gt $latest) {
          $latest = $child.LastWriteTimeUtc
        }
      }
    } elseif ($item.LastWriteTimeUtc -gt $latest) {
      $latest = $item.LastWriteTimeUtc
    }
  }
  $latest
}

function Test-FrontendDistUpToDate {
  $distIndex = Join-Path $RepoRoot "dist\index.html"
  if (-not (Test-Path -LiteralPath $distIndex)) {
    return $false
  }
  $frontendInputs = @(
    (Join-Path $RepoRoot "package.json"),
    (Join-Path $RepoRoot "package-lock.json"),
    (Join-Path $RepoRoot "tsconfig.json"),
    (Join-Path $RepoRoot "vite.config.ts"),
    (Join-Path $RepoRoot "index.html"),
    (Join-Path $RepoRoot "src"),
    (Join-Path $RepoRoot "public")
  )
  $latestInput = Get-LatestWriteTimeUtc -Paths $frontendInputs
  $distTime = (Get-Item -LiteralPath $distIndex).LastWriteTimeUtc
  $latestInput -ne [DateTime]::MinValue -and $distTime -ge $latestInput
}

if (-not $SkipPreflight) {
  Assert-RequiredPath "package.json" "npm package manifest"
  Assert-RequiredPath $TauriConfigPath "Tauri config"
  Assert-RequiredPath "public\pet\index.html" "pet static entry"
  Assert-RequiredPath "public\pet\pet.js" "pet static script"
  Assert-RequiredPath "data\tts\chattts_synth.py" "bundled ChatTTS synthesis script"
  Assert-RequiredPath "data\emoji\default" "bundled default emoji pack"
  Assert-RequiredPath "skills" "bundled skills directory"
  Assert-RequiredPath "node_modules" "node dependencies; run npm install first"

  $config = Get-Content -LiteralPath $TauriConfigPath -Raw | ConvertFrom-Json
  $webviewMode = $config.bundle.windows.webviewInstallMode.type
  if ($WebviewInstallMode -eq "config" -and $webviewMode -ne "offlineInstaller") {
    throw "Expected WebView2 offlineInstaller mode for fresh Windows packaging, got '$webviewMode'."
  }
  $resourceTargets = @($config.bundle.resources.PSObject.Properties | ForEach-Object { [string]$_.Value })
  if (($resourceTargets -notcontains "skills") -or ($resourceTargets -notcontains "public/pet") -or ($resourceTargets -notcontains "data/tts") -or ($resourceTargets -notcontains "data/emoji")) {
    throw "Tauri bundle.resources must include skills, public/pet, data/tts, and data/emoji."
  }
}

if ($null -ne $UpdateManifestUrl -and $UpdateManifestUrl.Trim().Length -gt 0) {
  $env:SYNTHCHAT_UPDATE_MANIFEST_URL = $UpdateManifestUrl.Trim()
  Write-Host "Using update manifest: $env:SYNTHCHAT_UPDATE_MANIFEST_URL"
}

if ($PreflightOnly) {
  Write-Host "Preflight complete."
  exit 0
}

if ($WebviewInstallMode -ne "config") {
  $OriginalTauriConfig = Get-Content -LiteralPath $TauriConfigPath -Raw
  $config = $OriginalTauriConfig | ConvertFrom-Json
  $config.bundle.windows.webviewInstallMode.type = $WebviewInstallMode
  if ($WebviewInstallMode -eq "skip") {
    if ($config.bundle.windows.webviewInstallMode.PSObject.Properties.Name -contains "silent") {
      $config.bundle.windows.webviewInstallMode.PSObject.Properties.Remove("silent")
    }
  } elseif ($config.bundle.windows.webviewInstallMode.PSObject.Properties.Name -contains "silent") {
    $config.bundle.windows.webviewInstallMode.silent = $true
  } else {
    $config.bundle.windows.webviewInstallMode | Add-Member -NotePropertyName "silent" -NotePropertyValue $true
  }
  Write-Utf8NoBom $TauriConfigPath ($config | ConvertTo-Json -Depth 20)
  Write-Host "Temporarily using WebView2 install mode: $WebviewInstallMode"
}

if ($FastIncremental) {
  $env:CARGO_INCREMENTAL = "1"
  $env:CARGO_PROFILE_RELEASE_INCREMENTAL = "true"
  if ([string]::IsNullOrWhiteSpace($env:CARGO_BUILD_JOBS)) {
    $env:CARGO_BUILD_JOBS = [Environment]::ProcessorCount.ToString()
  }
  Write-Host "Fast incremental mode: Cargo release incremental enabled, jobs=$env:CARGO_BUILD_JOBS"
}

$tauriArgs = @("run", "tauri", "--", "build")
if ($Bundle -ne "all") {
  $tauriArgs += @("--bundles", $Bundle)
}
if ($FastIncremental -and (Test-FrontendDistUpToDate)) {
  $FastConfigPath = Join-Path ([System.IO.Path]::GetTempPath()) ("synthchat-tauri-fast-build-{0}.json" -f ([Guid]::NewGuid().ToString("N")))
  $fastConfig = @{
    build = @{
      beforeBuildCommand = "cmd /c echo Skipping frontend build because dist is up to date."
    }
  }
  Write-Utf8NoBom $FastConfigPath ($fastConfig | ConvertTo-Json -Depth 8)
  $tauriArgs += @("--config", $FastConfigPath)
  Write-Host "Fast incremental mode: frontend dist is up to date; skipping npm run build."
} elseif ($FastIncremental) {
  Write-Host "Fast incremental mode: frontend dist is stale or missing; npm run build will run."
}

try {
  Write-Host "Building SynthChat native Windows package..."
  & npm @tauriArgs
  if ($LASTEXITCODE -ne 0) {
    $artifact = Get-LatestBundleArtifact -Bundle $Bundle
    $acceptArtifact = $false
    if ($AcceptExistingArtifactOnTimeout -and $null -ne $artifact) {
      $acceptArtifact = (
        $artifact.LastWriteTime -ge $BuildStartedAt.AddMinutes(-2) -and
        $artifact.Length -gt 1MB
      )
    }
    if ($acceptArtifact) {
      Write-Warning "Tauri returned exit code $LASTEXITCODE, but a fresh installer artifact exists: $($artifact.FullName)"
      Write-Warning "Accepting this artifact because -AcceptExistingArtifactOnTimeout was specified. If installation fails, rebuild with -WebviewInstallMode downloadBootstrapper."
      return
    }
    throw "Tauri build failed with exit code $LASTEXITCODE"
  }
} finally {
  if ($null -ne $FastConfigPath -and (Test-Path -LiteralPath $FastConfigPath)) {
    Remove-Item -LiteralPath $FastConfigPath -Force
  }
  if ($null -ne $OriginalTauriConfig) {
    Write-Utf8NoBom $TauriConfigPath $OriginalTauriConfig
    Write-Host "Restored Tauri config WebView2 mode."
  }
}

Write-Host "Build complete. Check src-tauri\target\release\bundle."
