# Cortex — Windows build, no-admin install, signing

## No-admin install ✅ (already configured)
`src-tauri/tauri.conf.json` → `bundle.windows.nsis.installMode = "currentUser"`. The NSIS
installer installs per-user under `%LOCALAPPDATA%` with **no administrator prompt**.
(WiX/MSI always needs admin, so distribute the **NSIS `-setup.exe`**, not the `.msi`.)

Build (on a Windows runner — cannot cross-build from Linux):
```
pnpm install && pnpm tauri build --bundles nsis
# → src-tauri/target/release/bundle/nsis/Cortex_<ver>_x64-setup.exe
```

## Signing (needs a certificate — the only external dependency)
Unsigned installers trip SmartScreen. Three options, cheapest-first:

1. **Azure Trusted Signing** (recommended, ~$10/mo, no hardware token). Set up a
   Trusted Signing account + cert profile, then add to `bundle.windows`:
   ```jsonc
   "signCommand": "trusted-signing-cli -e https://<region>.codesigning.azure.net -a <account> -c <profile> %1"
   ```
   (install `trusted-signing-cli`; auth via `AZURE_*` env / `az login` in CI.)
2. **OV cert** (~$200–400/yr): set `bundle.windows.certificateThumbprint` (cert in the
   machine store) + `digestAlgorithm: "sha256"` + `timestampUrl: "http://timestamp.digicert.com"`.
3. **EV cert** (~$300–700/yr, hardware token): instant SmartScreen trust; same config as OV
   but the token must be present on the signing machine (so CI needs a self-hosted runner).

Until a cert is provisioned the installer is functional but unsigned — fine for personal use.

## CI
`.github/workflows` carries a Windows build job (inert until a Windows runner is registered).
Once a runner + signing secret exist, it builds + signs the NSIS installer on tag push.

## Quick path — build + self-sign on Windows (no admin, works today)
Helper scripts (PowerShell, run on the Windows build box — NOT inside WSL; WSL is
Linux and would produce a Linux build):
```powershell
# one-time: create a self-signed code-signing cert in your user store (no admin)
pwsh -File scripts/make-selfsigned-cert.ps1        # prints THUMBPRINT
# build the per-user (no-admin) NSIS installer
pnpm install ; pnpm tauri build --bundles nsis
# sign it (sha256 + timestamp) — same command later for a real Azure/OV/EV cert
pwsh -File scripts/sign-windows.ps1 -Thumbprint <THUMBPRINT>
# -> src-tauri\target\release\bundle\nsis\Cortex_<ver>_x64-setup.exe  (signed)
```
Self-signed installs with no admin and runs, but SmartScreen warns on first launch
("More info -> Run anyway"). Swap in an Azure Trusted Signing thumbprint (see above)
for zero warnings — the sign step is identical.

## Publish a built+signed installer
Upload the signed `Cortex_<ver>_x64-setup.exe` to wherever you distribute releases
(e.g. a GitHub release, or your own download host).
