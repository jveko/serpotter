import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Production assets nest under /admin when served via ADMIN_SPA_DIR.
// Dev server stays at / with API proxy (base still /admin/ is fine for preview of nested paths).
export default defineConfig({
  base: "/admin/",
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/live": "http://127.0.0.1:8080",
      "/ready": "http://127.0.0.1:8080",
    },
  },
});
