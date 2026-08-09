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
// endpoints yet. Point at another instance with VELD_DAEMON_PORT; the `dev-ui`
// node in the repo's veld.json sets it to `${nodes.dev-daemon.port}`.
const daemonPort = process.env.VELD_DAEMON_PORT ?? "19898";

// Two ways this server gets started, and only one of them can pick a constant:
//
//   just dev-ui               the BOOTSTRAP tier — one worktree at a time, so
//                             5199 is fine and is what pty.rs's allowlist and
//                             `just dev-desktop` both hardcode.
//   veld start --preset dev   the dev stack as a veld run — veld allocates the
//                             port and hands it over as VELD_PORT, which is
//                             what lets two worktrees serve /ide at once.
//
// `strictPort` stays on in both. Under veld the port is not a preference but an
// allocation: something else already holds a reservation on it, the Caddy route
// in front of us names it, and silently sliding to the next one would serve the
// UI where nothing is looking for it.
const devPort = Number(process.env.VELD_PORT ?? "5199");

// Under veld this server is also reachable through Caddy at a hostname veld
// minted (`https://dev-ui.<run>.veld.localhost`). Vite 6 refuses a Host header
// it does not know, so the hostname has to be declared — and veld already told
// us it, in the same VELD_URL every long-running node gets. Deriving it here
// rather than taking a second env var keeps one definition point.
//
// Hand-parsed rather than `new URL`: this file's only ambient type is the
// `process` shim above, so there are no Node globals to lean on.
const veldHost = process.env.VELD_URL?.match(/^https?:\/\/([^/:]+)/)?.[1];

export default defineConfig({
  plugins: [react(), viteSingleFile()],
  build: {
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 4_000,
  },
  server: {
    port: devPort,
    strictPort: true,
    // Under veld, bind IPv4 loopback explicitly. Vite's default is `localhost`,
    // which on macOS resolves to `::1` — and veld's HTTP readiness probe checks
    // the port on both loopbacks but then fetches `http://127.0.0.1:<port>`
    // only (`wait_for_port` vs the `http` phase in veld-core's health.rs). So a
    // v6-only bind passes phase 1, fails phase 2 for the full 60s, and reports
    // "health check timed out" about a server that logged `ready in 176 ms`.
    // 127.0.0.1 is also what the Caddy upstream, VELD_DESKTOP_URL and
    // VELD_PROXY_ORIGINS all name, so this is the address the whole run agrees
    // on. Left alone off a veld run: `just dev-ui` has no probe, and its users
    // type `localhost:5199`.
    ...(process.env.VELD_PORT ? { host: "127.0.0.1" } : {}),
    // Only what veld actually routes to us. Left undefined off a veld run so
    // the bootstrap tier keeps vite's own localhost-only default — a blanket
    // `true` here would let any hostname resolving to 127.0.0.1 reach a dev
    // server that proxies the daemon's API.
    ...(veldHost ? { allowedHosts: [veldHost] } : {}),
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
