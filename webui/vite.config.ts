import fs from "node:fs";
import path from "node:path";

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, type Plugin } from "vite";

const devServerHost = process.env.DAAT_LOCUS_WEBUI_HOST ?? "0.0.0.0";
const daemonTarget = process.env.DAAT_LOCUS_DAEMON_URL ?? "http://0.0.0.0:53825";

const repositoryAssetsDir =
  process.env.DAAT_LOCUS_ASSETS_DIR ?? path.resolve(__dirname, "../assets");
const logoSvgPath = path.join(repositoryAssetsDir, "logo.svg");

function daatLocusLogoPlugin(): Plugin {
  return {
    name: "daat-locus-logo",
    configureServer(server) {
      server.middlewares.use("/logo.svg", (_request, response) => {
        if (!fs.existsSync(logoSvgPath)) {
          response.statusCode = 404;
          response.end("logo.svg not found");
          return;
        }

        response.setHeader("Content-Type", "image/svg+xml");
        response.setHeader("Cache-Control", "no-cache");
        fs.createReadStream(logoSvgPath).pipe(response);
      });
    },
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "logo.svg",
        source: fs.readFileSync(logoSvgPath),
      });
    },
  };
}

export default defineConfig({
  base: "./",
  plugins: [daatLocusLogoPlugin(), react(), tailwindcss()],
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
      "/config": {
        target: daemonTarget,
        changeOrigin: true,
      },
      "/sessions": {
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
    chunkSizeWarningLimit: 1_100,
  },
});
