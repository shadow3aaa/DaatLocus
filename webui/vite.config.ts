import path from "node:path";

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

const devServerHost = process.env.DAAT_LOCUS_WEBUI_HOST ?? "0.0.0.0";
const daemonTarget = process.env.DAAT_LOCUS_DAEMON_URL ?? "http://0.0.0.0:53825";

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    host: devServerHost,
    proxy: {
      "/status": {
        target: daemonTarget,
        changeOrigin: true,
      },
      "/dashboard": {
        target: daemonTarget,
        changeOrigin: true,
        ws: true,
      },
      "/settings": {
        target: daemonTarget,
        changeOrigin: true,
      },
      "/logs": {
        target: daemonTarget,
        changeOrigin: true,
      },
      "/commands": {
        target: daemonTarget,
        changeOrigin: true,
      },
      "/daemon": {
        target: daemonTarget,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: process.env.DAAT_LOCUS_WEBUI_OUT_DIR ?? "dist",
    emptyOutDir: true,
  },
});
