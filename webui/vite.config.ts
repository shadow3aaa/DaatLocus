import fs from "node:fs";
import path from "node:path";

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, type Plugin } from "vite";
import { mockPreviewPlugin } from "./mock/vite-plugin";

const devServerHost = process.env.DAAT_LOCUS_WEBUI_HOST ?? "127.0.0.1";
const daemonTarget = process.env.DAAT_LOCUS_DAEMON_URL ?? "http://127.0.0.1:45825";
const previewMock = process.env.DAAT_LOCUS_MOCK === "1";

const repositoryAssetsDir =
  process.env.DAAT_LOCUS_ASSETS_DIR ?? path.resolve(__dirname, "../assets");
const logoSvgPath = path.join(repositoryAssetsDir, "logo-mark.svg");

function daatLocusLogoPlugin(): Plugin {
  return {
    name: "daat-locus-logo",
    configureServer(server) {
      server.middlewares.use("/logo-mark.svg", (_request, response) => {
        if (!fs.existsSync(logoSvgPath)) {
          response.statusCode = 404;
          response.end("logo-mark.svg not found");
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
        fileName: "logo-mark.svg",
        source: fs.readFileSync(logoSvgPath),
      });
    },
  };
}

export default defineConfig({
  base: "/",
  plugins: [daatLocusLogoPlugin(), react(), tailwindcss(), ...(previewMock ? [mockPreviewPlugin()] : [])],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    host: devServerHost,
    proxy: previewMock ? undefined : {
      "/v1": {
        target: daemonTarget,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: process.env.DAAT_LOCUS_WEBUI_OUT_DIR ?? "dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 1_100,
    rolldownOptions: {
      output: {
        codeSplitting: true,
      },
    },
  },
});
