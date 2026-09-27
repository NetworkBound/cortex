# Cortex — Windows build, no-admin install, signing

## No-admin install (already configured)

`src-tauri/tauri.conf.json` → `bundle.windows.nsis.installMode = "currentUser"`. The NSIS
installer installs per-user under `%LOCALAPPDATA%` with **no administrator prompt**.
WiX/MSI always needs admin, so distribute the **NSIS `-setup.exe`**, not the `.msi`.

WebView2: the config uses Tauri's default `webviewInstallMode` (`downloadBootstrapper`),
so on a Windows 10 machine without WebView2 the installer fetches it (needs internet).
Windows 11 ships WebView2. If offline installs matter, set
`bundle.windows.webviewInstallMode` to `{ "type": "embedBootstrapper", "silent": true }`
(+~1.8 MB) or `offlineInstaller` (+~127 MB).

## Build on Windows

Prereqs: Visual Studio Build Tools with the C++ workload, Rust (MSVC toolchain), Node 20+
with pnpm, Go 1.26+ (the Tailscale sidecar), WebView2 runtime.

```powershell
pnpm install
# tauri.conf.json declares the sidecar as externalBin; tauri-build fails without it.
cd sidecar\cortex-tsnet
$env:CGO_ENABLED = "0"
go build -trimpath -ldflags="-s -w" -o ..\..\src-tauri\binaries\cortex-tsnet-x86_64-pc-windows-msvc.exe .
cd ..\..
# (or from Git Bash: bash scripts/build-tsnet-sidecar.sh x86_64-pc-windows-msvc)

$env:RUSTFLAGS = "--remap-path-prefix=$($env:USERPROFILE)="   # keep your home dir out of the binary
pnpm tauri build --bundles nsis
# -> src-tauri\target\release\bundle\nsis\Cortex_<ver>_x64-setup.exe
```

`--bundles msi,nsis` also produces the MSI if you want it. Run this in PowerShell on
Windows, not inside WSL (WSL is Linux and produces a Linux build).

### The certificateThumbprint gotcha

`tauri.conf.json` carries `bundle.windows.certificateThumbprint` pointing at the
maintainer's self-signed certificate. Tauri signs `cortex.exe`, `cortex-serve.exe`, the
sidecar and the installer with that cert during `tauri build`. On any machine where that
thumbprint is not in `Cert:\CurrentUser\My` the build dies with
`failed to run ... signtool.exe`. Either:

- replace the thumbprint with your own cert's (see below), or
- delete the key for an unsigned build:
  `jq 'del(.bundle.windows.certificateThumbprint)' src-tauri/tauri.conf.json`, or
  `pnpm tauri build --config '{"bundle":{"windows":{"certificateThumbprint":null}}}'`
  (Tauri merges `--config` over the file; `null` clears the field).

The CI workflows do the delete step automatically when no signing secret is configured.

## Cross-build from Linux / WSL (experimental)

`scripts/build-windows-msi.sh` uses `cargo-xwin` plus `makensis` to produce the NSIS
`-setup.exe` from Linux. It builds the Windows sidecar first, needs
`clang lld llvm nsis` from apt, and produces an **unsigned** installer (signtool is
Windows-only; the configured thumbprint is ignored on Linux because the bundler only
signs on Windows hosts). MSI cannot be cross-built. `scripts/build-portable.sh` is the
NSIS-free fallback: a zip with `cortex.exe`, `install.bat`, `uninstall.bat` that installs
per-user into `%LOCALAPPDATA%\Cortex`.

This path is not what releases use; the tag-based workflow builds on `windows-latest`.

## Signing

Unsigned installers trip SmartScreen. Options, cheapest first:

1. **Self-signed (free, works today).** Installs per-user without admin, but SmartScreen
   still warns on first run ("More info → Run anyway").
   ```powershell
   pwsh -File scripts/make-selfsigned-cert.ps1        # once; prints THUMBPRINT
   ```
   Put the thumbprint in `bundle.windows.certificateThumbprint` so `tauri build` signs
   every binary and the installer, **or** build unsigned and sign only the installer
   afterwards:
   ```powershell
   pwsh -File scripts/sign-windows.ps1 -Thumbprint <THUMBPRINT>
   ```
   Note the post-build script signs `*-setup.exe` and `*.msi` only; the `cortex.exe`
   inside stays unsigned. Prefer signing during the build.
2. **Azure Trusted Signing** (~$10/mo, no hardware token, SmartScreen reputation builds
   quickly). Install `trusted-signing-cli`, authenticate with `az login` / `AZURE_*`
   env vars, and replace `certificateThumbprint` with a `signCommand`:
   ```jsonc
   "windows": {
     "signCommand": "trusted-signing-cli -e https://<region>.codesigning.azure.net -a <account> -c <profile> -d Cortex %1",
     "digestAlgorithm": "sha256",
     "timestampUrl": "http://timestamp.digicert.com"
   }
   ```
   Tauri runs the command once per file with `%1` replaced by the path. Keep this in a
   separate `src-tauri/tauri.windows-signed.conf.json` and pass `--config` in CI if you
   don't want it in the base file.
3. **OV certificate** (~$200–400/yr): import the PFX into the store and set
   `certificateThumbprint` + `digestAlgorithm: "sha256"` +
   `timestampUrl: "http://timestamp.digicert.com"` (the last two are already set).
4. **EV certificate** (~$300–700/yr, hardware token): instant SmartScreen trust; the
   token must be attached to the signing machine, so CI needs a self-hosted runner.

## CI

- `.github/workflows/release.yml` (tag `v*.*.*`) builds on `windows-latest` alongside
  Linux and macOS and attaches the installer to a draft GitHub Release. If the repo
  secrets `WINDOWS_CERTIFICATE` (base64 PFX) and `WINDOWS_CERTIFICATE_PASSWORD` exist,
  the workflow imports the cert into the runner's `CurrentUser\My` store and points
  `certificateThumbprint` at it; otherwise it strips the field and ships unsigned.
- `.github/workflows/build-msi.yml` (manual, or on pushes to `main` touching the app)
  builds unsigned MSI + NSIS and uploads them as a workflow artifact.

## Portable zip (no installer at all)

`scripts/build-portable.sh` (from WSL/Linux) or hand-assemble: `cortex.exe` +
`scripts/portable/{install.bat,uninstall.bat,README.txt}`. `install.bat` copies the exe to
`%LOCALAPPDATA%\Cortex`, creates a Start Menu (and optional desktop) shortcut and
launches it; `uninstall.bat` removes those and leaves user data (`%USERPROFILE%\.cortex`,
`%LOCALAPPDATA%\cortex`) alone.
