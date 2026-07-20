import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// The production build is a single self-contained HTML file (JS, CSS, and
// fonts inlined) so veld-daemon can embed it with include_str! and serve it
// at /v2 with no external requests — see docs/branding.md.
export default defineConfig({
  plugins: [react(), viteSingleFile()],
  build: {
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 4_000,
  },
  server: {
    port: 5199,
    strictPort: true,
    proxy: {
      "/api": "http://127.0.0.1:19899",
    },
  },
  test: {
    environment: "node",
  },
});
