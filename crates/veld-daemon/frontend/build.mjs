import * as esbuild from "esbuild";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const isDev = process.argv.includes("--dev");

// Accept --outdir from build.rs (Cargo's OUT_DIR), default to ../assets for manual runs.
const outdirIdx = process.argv.indexOf("--outdir");
const assets = outdirIdx >= 0
  ? resolve(process.argv[outdirIdx + 1])
  : resolve(__dirname, "../assets");

const shared = {
  bundle: true,
  format: "iife",
  target: "es2020",
  minify: !isDev,
  sourcemap: isDev ? "linked" : false,
  logLevel: "info",
  loader: { ".css": "text" }, // Import CSS as string for shadow DOM injection
};

// JS entry points (CSS is bundled into feedback-overlay.js via import)
for (const { src, out } of [
  { src: "src/feedback-overlay/index.ts", out: "feedback-overlay.js" },
  { src: "src/draw-overlay/index.ts", out: "draw-overlay.js" },
  { src: "src/client-log/index.ts", out: "client-log.js" },
]) {
  await esbuild.build({
    ...shared,
    entryPoints: [resolve(__dirname, src)],
    outfile: resolve(assets, out),
  });
}
