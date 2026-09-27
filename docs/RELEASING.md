# Releasing Cortex

## What a release is

A `v*.*.*` tag. Pushing it runs `.github/workflows/release.yml`, which builds on
`ubuntu-22.04`, `windows-latest`, `macos-latest` (arm64 and x64), and attaches every
bundle to a **draft** GitHub Release named `Cortex vX.Y.Z`. You publish the draft by
hand. Current artifacts per release:

| Platform | Files |
|---|---|
| Linux | `Cortex_X.Y.Z_amd64.AppImage`, `Cortex_X.Y.Z_amd64.deb`, `Cortex-X.Y.Z-1.x86_64.rpm` |
| Windows | `Cortex_X.Y.Z_x64-setup.exe` (NSIS, per-user), `Cortex_X.Y.Z_x64_en-US.msi` |
| macOS | `Cortex_X.Y.Z_aarch64.dmg`, `Cortex_X.Y.Z_x64.dmg`, plus `.app.tar.gz` |

## Prereqs (one-time)

1. **Windows signing (optional).** Without it the Windows job builds unsigned. To sign
   in CI, add repo secrets `WINDOWS_CERTIFICATE` (base64 of a PFX) and
   `WINDOWS_CERTIFICATE_PASSWORD`. The workflow imports the cert and rewrites
   `bundle.windows.certificateThumbprint` to match. See `docs/WINDOWS-BUILD.md` for
   Azure Trusted Signing instead of a PFX.
2. **macOS signing / notarization (optional).** Not wired up. Gatekeeper warns on the
   unsigned `.dmg`. When there is a Developer ID: `APPLE_CERTIFICATE`,
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
   `APPLE_PASSWORD`, `APPLE_TEAM_ID` are the env vars `tauri-action` reads.
3. **Icons.** Already generated in `src-tauri/icons/`. To regenerate from a new
   1024x1024 source: `pnpm tauri icon src-tauri/icons/source.png`.
4. **AppImage self-update key (Linux, optional).** `commands/selfupdate.rs` verifies an
   ed25519 signature over each downloaded AppImage against the public key baked into
   the binary (`DEFAULT_UPDATE_PUBKEY`) or set via `~/.cortex/infra.json`
   `update_pubkey`. The matching private key lives only on the release machine. This
   updater targets a self-hosted Gitea release (`infra.json` `update_gitea_host`), not
   GitHub, and the signing/publish script is not part of this repo.

`TAURI_SIGNING_PRIVATE_KEY` / `createUpdaterArtifacts` are **not** used: Cortex does not
depend on `tauri-plugin-updater`. Don't add those secrets expecting `latest.json`.

## Release flow

1. Bump the version in **three** places, same string:
   `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`.
   (`Cargo.lock` is gitignored; `cargo check` refreshes it.)
2. Update `CHANGELOG.md` (one section per release; manual).
3. Make sure CI is green on `main`: `pnpm check`, `pnpm build`, `pnpm test` locally
   is the same set of gates.
4. Commit and push the bump.
5. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.
6. Watch the Release workflow. All four jobs must succeed for the draft to be complete;
   `fail-fast` is off, so one platform failing leaves the others attached.
7. Open the draft release, paste the changelog section, click **Publish**.
8. Running instances that have an update URL configured (Settings → Updates, or
   `localStorage` `cortex.updateUrl` pointing at the GitHub releases API) show the
   update pill on their next check. The Linux AppImage self-update only follows the
   Gitea host configured in `infra.json`.

## Notes on the Linux build

- Linux builds on `ubuntu-22.04` on purpose: the AppImage and `.deb` inherit the build
  host's glibc floor, and 22.04 is the oldest runner image with `libwebkit2gtk-4.1-dev`.
- `NO_STRIP=true` is set because linuxdeploy's bundled `strip` cannot parse `.relr.dyn`
  sections in current glibc/GTK and aborts the AppImage step.
- The stock Tauri AppImage bundles the build host's WebKit/GTK, which shows a black
  window on hosts with a much newer Mesa (Fedora 44+). `scripts/build-linux.sh` runs
  `scripts/fix-appimage-host-libs.sh` to repack the AppImage so it prefers the host's
  WebKit and keeps the bundled copy as a fallback. The CI release does **not** run that
  patch (tauri-action uploads inside its own step); if a release AppImage needs it,
  build with `scripts/build-linux.sh`, patch, and replace the asset on the release.

## Local test build

```bash
bash scripts/build-tsnet-sidecar.sh     # once per target triple
pnpm tauri:build:linux                  # Linux: deb + rpm + AppImage
pnpm tauri build --bundles nsis         # Windows (PowerShell): NSIS installer
pnpm tauri build --target aarch64-apple-darwin   # macOS
```

Outputs are under `src-tauri/target/<triple or release>/bundle/`. The Linux→Windows
cross-build (`scripts/build-windows-msi.sh`, cargo-xwin + NSIS) works for the NSIS
installer but is not the release path; use the workflow.
