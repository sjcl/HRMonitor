import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

// In production nginx serves `dist/` and proxies `/api/*`, so the browser only
// ever sees one origin and no build-time API base URL is needed. The dev proxy
// below reproduces that same-origin arrangement against locally running
// services, which keeps cookie behaviour identical between dev and prod.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 3000,
    proxy: {
      // Order matters: the WebSocket gateway is a separate service and its
      // paths sit underneath /api, so it has to be matched first.
      "/api/ws": {
        target: "ws://localhost:3002",
        ws: true,
        changeOrigin: false,
      },
      "/api": {
        target: "http://localhost:3001",
        changeOrigin: false,
      },
      "/healthz": {
        target: "http://localhost:3001",
        changeOrigin: false,
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
