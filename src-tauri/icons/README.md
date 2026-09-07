# Icons

Placeholder. Before first `tauri build`, generate icons from a 1024x1024 source PNG with:

```bash
pnpm tauri icon path/to/source.png
```

This writes `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns` (macOS), and `icon.ico` (Windows) into this directory. The paths match `tauri.conf.json` `bundle.icon`.

Until then, `tauri dev` works (icons only required for `tauri build`).
