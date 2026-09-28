# Icons

Generated from `source.png` (1024x1024) with:

```bash
pnpm tauri icon src-tauri/icons/source.png
```

That writes the desktop set (`32x32.png`, `128x128.png`, `128x128@2x.png`,
`icon.png`, `icon.icns` for macOS, `icon.ico` for Windows), the Windows Store
`Square*Logo.png` / `StoreLogo.png` tiles and the `android/` + `ios/` sets into
this directory. The paths match `tauri.conf.json` `bundle.icon`.

To change the app icon, replace `source.png` and re-run the command; do not
hand-edit the derived files.
