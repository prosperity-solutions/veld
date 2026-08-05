/**
 * A QR symbol as an SVG, for opening a public share link on a phone.
 *
 * Deliberately **black on white in both themes**. Everything else in this app follows
 * the product tokens (`docs/branding.md`), and this is the one surface that must not:
 * a scanner needs a light quiet zone and high contrast, and a dark-theme QR in
 * `--bg2` with `--text` modules is a code that reads on some phones and not others.
 *
 * The encoder is `shared/qr.ts` — written rather than installed, because
 * `vite-plugin-singlefile` inlines every asset into the daemon binary.
 *
 * **Before rendering this somewhere new, check what the payload carries.** A web-share
 * link includes the share password in its fragment (`Sharing.tsx` → `webShareLink`), so
 * a code drawn on an always-visible surface is a credential on permanent display. The
 * sharing panel handles that with the `.qr-shield` blur (hover, or *Show all*) in
 * `styles.css`; anywhere ambient needs its own gate rather than inheriting this
 * component's silence about it.
 */

import { Text } from "@mantine/core";

import {
  QR_SCREEN_QUIET_ZONE,
  VELD_MARK,
  encodeQr,
  qrPath,
  qrRenderSize,
  qrViewBox,
  veldMarkBox,
} from "./qr";

/**
 * The veld mark on a white plate at the centre of the symbol.
 *
 * Inlined rather than referenced: the daemon serves one self-contained HTML file (see
 * `docs/branding.md`), so an `<image href>` to an asset would be a request this page
 * cannot make. Geometry and path data come from [`VELD_MARK`], shared with the canvas
 * renderer behind *Copy QR* so the two cannot draw different codes.
 */
function VeldMark(props: { symbolSize: number; quiet: number }) {
  const { side, origin } = veldMarkBox(props.symbolSize, props.quiet);
  const scale = (side - 2 * VELD_MARK.pad) / VELD_MARK.viewBox;
  return (
    <g>
      <rect
        x={origin}
        y={origin}
        width={side}
        height={side}
        rx={side * 0.22}
        fill="#ffffff"
      />
      <g
        transform={`translate(${origin + VELD_MARK.pad} ${origin + VELD_MARK.pad}) scale(${scale})`}
      >
        <path d={VELD_MARK.glyph} fill="#000000" />
        <path d={VELD_MARK.dot} fill={VELD_MARK.dotFill} />
      </g>
    </g>
  );
}

export function QrCode(props: {
  value: string;
  /** Target side in CSS pixels; the actual side snaps to an integer module scale. */
  size?: number;
  /** Accessible name — a QR is an image of a URL, so say which one. */
  label: string;
  /** Draw the veld mark at the centre. On by default; see [`VELD_MARK`]. */
  logo?: boolean;
}) {
  const qr = encodeQr(props.value);
  if (!qr) {
    // Only reachable past 213 bytes. Saying so beats rendering a 57×57 grid at 2px a
    // module that no camera can resolve — and beats saying nothing, which would look
    // like a broken image.
    return (
      <Text size="xs" c="dimmed">
        This link is too long for a QR code — use Copy link instead.
      </Text>
    );
  }
  const quiet = QR_SCREEN_QUIET_ZONE;
  const box = qrViewBox(qr, quiet);
  const side = qrRenderSize(box, props.size ?? 108);
  return (
    <svg
      width={side}
      height={side}
      viewBox={`0 0 ${box} ${box}`}
      role="img"
      aria-label={props.label}
      // `shapeRendering: crispEdges` keeps module edges on pixel boundaries; without
      // it a fractional scale anti-aliases every edge into grey and the contrast a
      // scanner needs goes with it.
      style={{
        background: "#ffffff",
        borderRadius: 4,
        display: "block",
        shapeRendering: "crispEdges",
      }}
    >
      {/* The quiet zone is part of the symbol, so the white must cover the whole
          viewBox rather than only the module area. */}
      <rect width={box} height={box} fill="#ffffff" />
      <path d={qrPath(qr, quiet)} fill="#000000" />
      {/* Drawn last, over the modules it replaces — the decoder repairs those from the
          error-correction codewords. `crispEdges` is deliberately not inherited here:
          the mark is curves, not a grid. */}
      {props.logo !== false && (
        <g style={{ shapeRendering: "auto" }}>
          <VeldMark symbolSize={qr.size} quiet={quiet} />
        </g>
      )}
    </svg>
  );
}
