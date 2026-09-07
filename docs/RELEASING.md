# Releasing Cortex

## Prereqs (one-time)

1. **Tauri updater signing keys.**
   ```bash
   pnpm tauri signer generate -w ~/.tauri-cortex.key
   ```
   This emits a public key (paste into `src-tauri/tauri.conf.json` under
   `plugins.updater.pubkey`) and a private key file. Put the private key
   into GitHub repo secrets as `TAURI_SIGNING_PRIVATE_KEY` and the
   passphrase as `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

2. **Icons.** Either drop a real 1024x1024 `src-tauri/icons/source.png`
   and run `pnpm tauri icon src-tauri/icons/source.png`, or run
   `./scripts/gen-placeholder-icon.sh` for a quick "H" badge.

3. **Code-signing certificates** (optional but recommended before public
   distribution):
   - **Windows:** Buy or use a Sectigo / DigiCert EV cert. Add cert + password
     to repo secrets as `WINDOWS_CERTIFICATE` (base64-encoded PFX) and
     `WINDOWS_CERTIFICATE_PASSWORD`. Wire into the workflow via
     `tauri-action`'s `wixSigningArgs`.
   - **macOS:** Apple Developer ID Application cert in the runner's keychain.
     Notarize via `xcrun notarytool` step.
   - **Linux:** Optional gpg signing of the .deb / .AppImage. Skip for v1.

## Release flow

1. Bump version in `package.json` and `src-tauri/Cargo.toml`. Same version
   string.
2. Update `CHANGELOG.md` (one section per release; manual until we add an
   auto-changelog tool).
3. Commit + push the bump.
4. Tag: `git tag v0.1.0 && git push --tags`.
5. The Release workflow (`.github/workflows/release.yml`) fires:
   matrix-builds on Linux/Windows/macOS, signs (when secrets present),
   creates a draft GH Release with the artifacts attached.
6. Open the draft release, write notes, click **Publish**.
7. The auto-updater on running instances will discover the new
   `latest.json` on next launch.

## Tauri updater wiring

Once Phase 6 is in, add to `src-tauri/Cargo.toml`:

```toml
tauri-plugin-updater = "2"
```

And in `lib.rs`:

```rust
.plugin(tauri_plugin_updater::Builder::new().build())
```

And in `tauri.conf.json` under `plugins.updater`:

```json
{
  "active": true,
  "endpoints": ["https://github.com/<you>/cortex/releases/latest/download/latest.json"],
  "pubkey": "<paste from step 1>"
}
```

Renderer side: invoke `plugin:updater|check` and prompt before installing.

## Local test build

```bash
pnpm tauri:build
# Outputs under src-tauri/target/release/bundle/...
```

For per-target:
```bash
pnpm tauri:build --target x86_64-pc-windows-msvc   # from a Windows runner
pnpm tauri:build --target aarch64-apple-darwin     # from macOS
```

WSL2 → Windows cross-compile is theoretically possible via
`cross` + `cargo-xwin` but is not part of the supported flow. Use GHA.
