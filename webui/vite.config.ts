import path from "node:path";

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

const daemonTarget = process.env.DAAT_LOCUS_DAEMON_URL ?? "http://127.0.0.1:53825";

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
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
    outDir: "dist",
    emptyOutDir: true,
  },
});
