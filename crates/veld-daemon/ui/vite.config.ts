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
// `||` for the same reason as `devPort` below: `just dev-ui` clears this by
// assigning empty, and `"" ?? "19898"` is `""` — which would proxy /api to
// `http://127.0.0.1:` and fail every request with no useful message.
const daemonPort = process.env.VELD_DAEMON_PORT || "19898";

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
// `||`, not `??`. The bootstrap recipes clear these variables by assigning the
// empty string (a `just` recipe cannot unset one), and `"" ?? "5199"` is `""`,
// which `Number` turns into 0 — vite would bind a random port and `strictPort`
// would not save you, because 0 is a port it was legitimately asked for.
const devPort = Number(process.env.VELD_PORT || "5199");

// Under veld this server is also reachable through Caddy at a hostname veld
// minted (`https://dev-ui.<run>.veld.localhost`), which is declared below.
//
// A FORWARD GUARD, not a present requirement — worth being honest about, since
// the obvious reading is that it is load-bearing. Vite 6's host check allows any
// IPv4 literal and anything ending in `.localhost` before it consults
// `allowedHosts`, and every hostname this repo's `url_template` mints ends in
// `.veld.localhost`. So the entry is inert today and becomes load-bearing the
// moment `url_template` moves off `.localhost`. Derived from the VELD_URL every
// long-running node already gets, rather than a second env var.
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
    // VELD_DESKTOP_URL and VELD_PROXY_ORIGINS name 127.0.0.1 too, so the run
    // agrees on one address. (The Caddy upstream is the exception — veld writes
    // `localhost:<port>` there — but Go's dialer falls back from `::1`, so a
    // v4-only bind is still reachable through it. The readiness probe is what
    // decides this, not Caddy.) Left alone off a veld run: `just dev-ui` has no
    // probe, and its users type `localhost:5199`.
    ...(process.env.VELD_PORT ? { host: "127.0.0.1" } : {}),
    // Only what veld actually routes to us. Left undefined off a veld run so
    // the bootstrap tier keeps vite's own default — a blanket `true` here would
    // let any hostname resolving to 127.0.0.1 reach a dev server that proxies
    // the daemon's API.
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
  // Two test projects, split by file extension, because a component test needs
  // a DOM and 46 module tests do not.
  //
  // **Why not simply `environment: "jsdom"` globally.** It works — all 1275
  // existing tests pass under it — but it costs every suite jsdom's per-file
  // construction whether or not the suite touches the DOM. Measured on this
  // tree at `--maxWorkers=4` (roughly a CI runner's parallelism), two runs each:
  //
  //     environment: "node"     4.3s / 4.9s      (what this repo had)
  //     environment: "jsdom"   11.2s / 12.6s     2.6x, for no extra coverage
  //     these two projects      4.7s / 5.1s
  //
  // The middle row buys nothing the bottom row doesn't: the same component
  // tests, in the same jsdom. It just also taxes the 46 files that never look
  // at a `document`.
  //
  // **Why by extension rather than a per-file docblock.** The sibling package
  // `crates/veld-daemon/frontend` reaches the same place with
  // `// @vitest-environment jsdom` at the top of each of its ~19 DOM tests, and
  // that is a perfectly good answer there. Here the extension already carries
  // the signal — a `.tsx` test exists *because* it renders a component — so
  // deriving the environment from it leaves nothing to remember. Either scheme
  // fails loudly rather than silently when you get it wrong — a forgotten
  // docblock over there gives `document is not defined`; a render test saved as
  // `.test.ts` here dies earlier still, at transform, because esbuild does not
  // parse JSX in a `.ts` file. So this is ergonomics, not a correctness gate.
  //
  // `extends: true` is what gives each project the root config above, the
  // `react()` plugin included — without it a `.tsx` test has no JSX transform.
  //
  // **The two `include`s must partition vitest's default pattern exactly, and
  // that is the whole subtlety here.** A file matched by *neither* project is
  // not an error — it is simply never collected, so a broken test in it reports
  // green and exits 0. This was got wrong twice while writing it: first scoped
  // to `src/**` (a suite in `tests/`, where the sibling package keeps its own,
  // would have vanished), then narrowed to `.test.ts`/`.test.tsx` (a `.spec.ts`
  // or `.test.mts` vanished — measured: two deliberately failing files, "48
  // passed", exit 0). So these mirror vitest's own default,
  // `**/*.{test,spec}.?(c|m)[jt]s?(x)`, split on the one thing that decides the
  // environment: the trailing `x`. JSX means a component, a component means a
  // DOM. Widen both together or neither.
  //
  // `exclude` **replaces** vitest's defaults rather than extending them, which
  // is why these are spelled `**/`-anchored — an unanchored `node_modules/**`
  // would only match at the package root and let a nested one be crawled.
  test: {
    projects: [
      {
        extends: true,
        test: {
          name: "unit",
          environment: "node",
          include: ["**/*.{test,spec}.?(c|m)[jt]s"],
          exclude: ["**/node_modules/**", "**/dist/**"],
        },
      },
      {
        extends: true,
        test: {
          name: "dom",
          environment: "jsdom",
          include: ["**/*.{test,spec}.?(c|m)[jt]sx"],
          exclude: ["**/node_modules/**", "**/dist/**"],
          // Unmounts between tests and stubs the browser APIs jsdom is missing.
          // Not optional — see that file for what breaks without it.
          setupFiles: ["./src/shared/testSetup.ts"],
        },
      },
    ],
  },
});
