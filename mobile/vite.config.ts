import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

// Cortex mobile SPA.
//
// In production the embedded Cortex server serves the contents of `mobile/dist`
// at the SAME origin as the API, so every fetch path is relative (`/api/...`)
// and the WebSocket is derived from `location.host`. `base: './'` keeps the
// built asset URLs relative. The same bundle is wrapped by Capacitor
// (mobile/native), where the API base comes from pairing instead.
//
// PWA bits (manifest.webmanifest, sw.js) are plain files in `public/` — no
// plugin, so the service worker is small and readable.
//
// In dev, set `VITE_API_BASE` (e.g. `http://localhost:8788`) to point the
// dev server's `/api` + `/ws` at a running Cortex; we proxy both so the SPA
// keeps using same-origin relative paths during development.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const apiBase = env.VITE_API_BASE || "";

  return {
    base: "./",
    plugins: [react()],
    build: {
      outDir: "dist",
      emptyOutDir: true,
      target: "es2020",
      sourcemap: false,
    },
    server: {
      host: true,
      proxy: apiBase
        ? {
            "/api": { target: apiBase, changeOrigin: true },
            "/ws": { target: apiBase, ws: true, changeOrigin: true },
          }
        : undefined,
    },
  };
});
