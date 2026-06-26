import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// Cortex mobile SPA.
//
// In production the embedded Cortex server serves the contents of `mobile/dist`
// at the SAME origin as the API, so every fetch path is relative (`/api/...`)
// and the WebSocket is derived from `location.host`. We therefore set
// `base: './'` so the built asset URLs are relative and survive being served
// from any mount point.
//
// In dev, set `VITE_API_BASE` (e.g. `http://localhost:8788`) to point the
// dev server's `/api` + `/ws` at a running Cortex; we proxy both so the SPA
// keeps using same-origin relative paths during development.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const apiBase = env.VITE_API_BASE || "";

  return {
    base: "./",
    plugins: [
      react(),
      VitePWA({
        registerType: "autoUpdate",
        includeAssets: ["favicon.svg"],
        manifest: {
          name: "Cortex",
          short_name: "Cortex",
          description: "Cortex mobile — multi-model agent client",
          theme_color: "#0a0a0b",
          background_color: "#0a0a0b",
          display: "standalone",
          orientation: "portrait",
          start_url: "./",
          scope: "./",
          icons: [
            {
              src: "icon-192.png",
              sizes: "192x192",
              type: "image/png",
            },
            {
              src: "icon-512.png",
              sizes: "512x512",
              type: "image/png",
            },
            {
              src: "icon-512.png",
              sizes: "512x512",
              type: "image/png",
              purpose: "maskable",
            },
          ],
        },
        workbox: {
          // Never cache the API or WS — only the app shell.
          navigateFallbackDenylist: [/^\/api/, /^\/ws/],
          globPatterns: ["**/*.{js,css,html,svg,png,ico,woff2}"],
        },
      }),
    ],
    build: {
      outDir: "dist",
      emptyOutDir: true,
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
