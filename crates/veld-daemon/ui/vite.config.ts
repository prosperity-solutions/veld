import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// The production build is a single self-contained HTML file (JS, CSS, and
// fonts inlined) so veld-daemon can embed it with include_str! and serve it
// at /ide with no external requests — see docs/branding.md.
// Vite runs in Node; only this config file sees `process`.
declare const process: { env: Record<string, string | undefined> };

// Dev default is the DEV daemon instance (`just dev-daemon`, port 19898) —
// during development the installed daemon usually doesn't carry the desktop
// endpoints yet. Point at another instance with VELD_DAEMON_PORT.
const daemonPort = process.env.VELD_DAEMON_PORT ?? "19898";

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
      // `ws: true` so the terminal's `/api/pty/attach` upgrade is proxied
      // too; without it vite answers the handshake itself and the socket
      // never reaches the daemon. The daemon only trusts this dev origin
      // when it is a dev instance (see `allowed_origins` in pty.rs), so
      // `just dev-ui` must point at `just dev-daemon`, not the installed one.
      "/api": { target: `http://127.0.0.1:${daemonPort}`, ws: true },
    },
  },
  test: {
    environment: "node",
  },
});
