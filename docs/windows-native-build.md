# Windows Native Build

SynthChat uses Tauri release builds for native Windows packaging.

## Build

From the repository root, the one-click wrapper is:

```powershell
.\build-one-click.ps1
```

For double-click usage, run `build-one-click.cmd`. The wrapper auto-detects the GitHub Releases update source from `origin`, keeps WebView2 in silent `offlineInstaller` mode, calls the native build script, and copies installer artifacts into `release-dist/`.

Run the preflight checks first:

```powershell
npm run desktop:build:windows -- -PreflightOnly
```

```powershell
npm run desktop:build:windows -- -Bundle nsis
```

To inject a default GitHub Releases update source into the app:

```powershell
npm run desktop:build:windows -- -Bundle nsis -UpdateManifestUrl "https://api.github.com/repos/<owner>/<repo>/releases/latest"
```

If WebView2 `offlineInstaller` bundling times out, build with the bootstrapper fallback:

```powershell
npm run desktop:build:windows -- -Bundle nsis -WebviewInstallMode downloadBootstrapper
```

That fallback still works on a fresh Windows machine when the installer has internet access to fetch WebView2.

The one-click wrapper accepts fresh installer artifacts when Tauri reports `failed to bundle project timeout: global` after writing the NSIS installer. To force strict bundler exit-code handling, pass `-StrictBundlerExitCode`. To automatically retry with the smaller WebView2 bootstrapper fallback, pass `-RetryWithDownloadBootstrapper`.

## GitHub Releases Update Source

1. Create a GitHub Release whose tag is the app version, for example `v1.1.1`.
2. Upload the Windows installer asset, for example `SynthChat_1.1.1_x64-setup.exe`.
3. In SynthChat Settings -> About, set the update source to:

```powershell
https://api.github.com/repos/<owner>/<repo>/releases/latest
```

4. Click "检查更新" to detect the latest release.
5. Click "下载新版本" to open the release/asset download URL.
6. Click "下载并静默安装" to download the installer, close SynthChat, and run the installer silently.

The latest release tag must be greater than the currently installed version. If the installed app is `1.1.0`, a release tagged `v1.1.0` is treated as already current; use `v1.1.1` or higher to test updating.

## Packaging Notes

- Release builds use `windows_subsystem = "windows"` in `src-tauri/src/main.rs`, so the app does not open an extra console window.
- `src-tauri/tauri.conf.json` defaults to WebView2 `offlineInstaller` with `silent: true`, which is heavier but is the desired fully native fresh-Windows mode.
- `downloadBootstrapper` is the practical fallback when Tauri cannot finish embedding the offline WebView2 installer in the current environment.
- `bundle.resources` explicitly includes `skills`, `public/pet`, and `data/tts`; `data/tts/chattts_synth.py` travels with the app, while Vite also copies `public` into `dist` for normal frontend loading.
- The About page checks either the saved manifest URL or the build-time `SYNTHCHAT_UPDATE_MANIFEST_URL`.
- Silent replacement install is supported for `.exe`, `.msi`, and `.msix` assets. The NSIS `.exe` path uses `/S`, which Tauri NSIS installers support.
- Windows UAC or SmartScreen may still show system prompts if the package is unsigned or requires elevation.

## ChatTTS Packaging Mode

SynthChat uses the lightweight recommended ChatTTS mode:

- The installer bundles the SynthChat app and `data/tts/chattts_synth.py`.
- The installer does not bundle the full `E:\SynthChat\ChatTTS` model/runtime directory.
- ChatTTS model files, Python, `ChatTTS`, `torch`, `torchaudio`, `numpy`, and `ffmpeg` remain external user/runtime dependencies.
- Runtime discovery checks configured paths first, then common local paths such as `ChatTTS`, `models/ChatTTS`, and Tauri resource paths.

For a fresh Windows machine, prepare the external ChatTTS directory and Python environment separately, then set the voice reply `模型目录` / `Python 路径` in the persona settings if auto-discovery does not find them.

## Update Manifest

The checker accepts `docs/update-manifest.example.json` shape, and also accepts GitHub Releases API responses that include `tag_name`, `html_url`, `body`, `published_at`, and `assets`.
