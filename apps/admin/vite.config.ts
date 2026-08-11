import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite-plus";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";

const root = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  // Served at the site root by the Rust API (ADMIN_SPA_DIR). Leave base at the
  // default "/" — a sub-path base breaks the SPA deep-link fallback.
  plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react()],
  resolve: {
    alias: { "@": path.resolve(root, "src") },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/live": "http://127.0.0.1:8080",
      "/ready": "http://127.0.0.1:8080",
    },
  },
  // Vitest (npm test). Logic-level unit tests only — no heavy Base UI renders.
  // vite-plus's defineConfig passes `test` through to vitest.
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
  // Plugin regenerates this file; oxfmt must not thrash quotes vs generator.
  fmt: {
    ignorePatterns: ["src/routeTree.gen.ts"],
  },
});
