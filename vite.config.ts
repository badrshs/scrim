import { defineConfig } from "vite";

// Tauri serves ui/ in dev and bundles ui/dist in release. Nothing here may
// reach for a CDN at runtime: Scrim is an offline app and the only network
// traffic it is allowed to make is a cast stream to a device on the LAN.
export default defineConfig({
  root: "ui",
  publicDir: "../ui/public",
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "chrome110",
    sourcemap: true,
    assetsInlineLimit: 0,
  },
});
