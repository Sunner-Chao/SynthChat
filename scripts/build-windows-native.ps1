param(
  [string]$UpdateManifestUrl = $env:SYNTHCHAT_UPDATE_MANIFEST_URL,
  [ValidateSet("all", "nsis", "msi")]
  [string]$Bundle = "all",
  [ValidateSet("config", "offlineInstaller", "embedBootstrapper", "downloadBootstrapper", "skip")]
  [string]$WebviewInstallMode = "config",
  [switch]$SkipPreflight,
  [switch]$PreflightOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $RepoRoot
$TauriConfigPath = "src-tauri\tauri.conf.json"
$OriginalTauriConfig = $null

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
  [System.IO.File]::WriteAllText((Resolve-Path -LiteralPath $Path), $Value, $encoding)
}

if (-not $SkipPreflight) {
  Assert-RequiredPath "package.json" "npm package manifest"
  Assert-RequiredPath $TauriConfigPath "Tauri config"
  Assert-RequiredPath "public\pet\index.html" "pet static entry"
  Assert-RequiredPath "public\pet\pet.js" "pet static script"
  Assert-RequiredPath "data\tts\chattts_synth.py" "bundled ChatTTS synthesis script"
  Assert-RequiredPath "skills" "bundled skills directory"
  Assert-RequiredPath "node_modules" "node dependencies; run npm install first"

  $config = Get-Content -LiteralPath $TauriConfigPath -Raw | ConvertFrom-Json
  $webviewMode = $config.bundle.windows.webviewInstallMode.type
  if ($WebviewInstallMode -eq "config" -and $webviewMode -ne "offlineInstaller") {
    throw "Expected WebView2 offlineInstaller mode for fresh Windows packaging, got '$webviewMode'."
  }
  $resourceTargets = @($config.bundle.resources.PSObject.Properties | ForEach-Object { [string]$_.Value })
  if (($resourceTargets -notcontains "skills") -or ($resourceTargets -notcontains "public/pet") -or ($resourceTargets -notcontains "data/tts")) {
    throw "Tauri bundle.resources must include skills, public/pet, and data/tts."
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

$tauriArgs = @("run", "tauri", "--", "build")
if ($Bundle -ne "all") {
  $tauriArgs += @("--bundles", $Bundle)
}

try {
  Write-Host "Building SynthChat native Windows package..."
  & npm @tauriArgs
  if ($LASTEXITCODE -ne 0) {
    throw "Tauri build failed with exit code $LASTEXITCODE"
  }
} finally {
  if ($null -ne $OriginalTauriConfig) {
    Write-Utf8NoBom $TauriConfigPath $OriginalTauriConfig
    Write-Host "Restored Tauri config WebView2 mode."
  }
}

Write-Host "Build complete. Check src-tauri\target\release\bundle."
