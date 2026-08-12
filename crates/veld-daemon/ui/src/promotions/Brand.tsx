/**
 * The `veld.` wordmark and the promotion glyph set.
 *
 * The wordmark is the brand mark for this surface. `docs/branding.md` keeps it
 * out of the `/ide` top bar — 40–42px of pure controls, which is the documented
 * tight-chrome exception — but that exception is explicitly about *chrome*: "a
 * page, panel, dialog or header has room for the wordmark and takes it". The
 * start screen is a full page and the what's-new panel is a dialog, so both take
 * it, and the app's brand stops riding on the favicon alone at the one moment a
 * new user is deciding what this thing is.
 */

/**
 * `veld.`, coloured from the theme.
 *
 * Paths are `crates/veld-daemon/assets/logo-wordmark.svg`, reordered so the dot
 * is **last** — the branding doc's rule is `path:last-child` takes the accent —
 * and with their hard-coded `fill` attributes dropped so CSS owns the colour in
 * both themes. Inline rather than an `<img>`: the `/ide` bundle is one
 * self-contained file and a themed mark cannot be a raster.
 */
export function Wordmark(props: { height?: number; className?: string }) {
  const height = props.height ?? 34;
  return (
    <svg
      className={`wordmark${props.className ? ` ${props.className}` : ""}`}
      height={height}
      viewBox="0 0 3837 1463"
      role="img"
      aria-label="veld"
      focusable="false"
    >
      <path d="M383 1463L0 423H183L470 1278H474L762 423H943L561 1463H383Z" />
      <path d="M1507 1463C1407.67 1463 1322.17 1441 1250.5 1397C1178.83 1353 1123.83 1290.83 1085.5 1210.5C1047.17 1130.17 1028 1035.67 1028 927V926C1028 818.667 1047.33 724.167 1086 642.5C1124.67 560.833 1179 497.167 1249 451.5C1319 405.833 1401.33 383 1496 383C1590.67 383 1672.17 404.833 1740.5 448.5C1808.83 492.167 1861.33 553.333 1898 632C1934.67 710.667 1953 802 1953 906V970H1115V834H1867L1779 960V893C1779 812.333 1766.83 745.667 1742.5 693C1718.17 640.333 1684.67 601.167 1642 575.5C1599.33 549.833 1550.33 537 1495 537C1439.67 537 1390 550.5 1346 577.5C1302 604.5 1267.33 644.5 1242 697.5C1216.67 750.5 1204 815.667 1204 893V960C1204 1033.33 1216.5 1096 1241.5 1148C1266.5 1200 1302 1239.83 1348 1267.5C1394 1295.17 1448.33 1309 1511 1309C1555 1309 1594.33 1302.33 1629 1289C1663.67 1275.67 1692.67 1257.33 1716 1234C1739.33 1210.67 1756 1184 1766 1154L1769 1145H1940L1938 1155C1929.33 1197.67 1912.83 1237.67 1888.5 1275C1864.17 1312.33 1833 1345.17 1795 1373.5C1757 1401.83 1713.67 1423.83 1665 1439.5C1616.33 1455.17 1563.67 1463 1507 1463Z" />
      <path d="M2137 1463V20H2311V1463H2137Z" />
      <path d="M2937 1463C2848.33 1463 2770.83 1440.83 2704.5 1396.5C2638.17 1352.17 2586.67 1289.5 2550 1208.5C2513.33 1127.5 2495 1032.33 2495 923V922C2495 812.667 2513.5 717.667 2550.5 637C2587.5 556.333 2639 493.833 2705 449.5C2771 405.167 2847.33 383 2934 383C2983.33 383 3029.17 390.833 3071.5 406.5C3113.83 422.167 3151.67 444.5 3185 473.5C3218.33 502.5 3245.67 537 3267 577H3271V0H3445V1443H3271V1267H3267C3245.67 1307.67 3218.67 1342.5 3186 1371.5C3153.33 1400.5 3116.17 1423 3074.5 1439C3032.83 1455 2987 1463 2937 1463ZM2971 1309C3029.67 1309 3081.67 1293 3127 1261C3172.33 1229 3207.83 1184 3233.5 1126C3259.17 1068 3272 1000.33 3272 923V922C3272 844.667 3259 777.167 3233 719.5C3207 661.833 3171.5 617 3126.5 585C3081.5 553 3029.67 537 2971 537C2909.67 537 2856.67 552.667 2812 584C2767.33 615.333 2733 659.667 2709 717C2685 774.333 2673 842.667 2673 922V923C2673 1002.33 2685 1071 2709 1129C2733 1187 2767.33 1231.5 2812 1262.5C2856.67 1293.5 2909.67 1309 2971 1309Z" />
      <path d="M3757 1463C3801.18 1463 3837 1427.18 3837 1383C3837 1338.82 3801.18 1303 3757 1303C3712.82 1303 3677 1338.82 3677 1383C3677 1427.18 3712.82 1463 3757 1463Z" />
    </svg>
  );
}

/**
 * The glyph set, drawn in `currentColor` at a 24-box.
 *
 * Line art, never a raster and never a screenshot — see `model.ts`. Stroke-only
 * so both themes get a correct mark from one definition, and so the whole set
 * costs a few hundred bytes in a bundle that inlines everything it ships.
 */
const GLYPH_PATHS: Record<string, React.ReactNode> = {
  terminal: (
    <>
      <rect x="2.75" y="4.75" width="18.5" height="14.5" rx="2" />
      <path d="M7 10l3 2.4-3 2.4" />
      <path d="M12.5 15.4h4.5" />
    </>
  ),
  panes: (
    <>
      <rect x="2.75" y="4.75" width="18.5" height="14.5" rx="2" />
      <path d="M10.5 4.75v14.5" />
      <path d="M10.5 12h10.75" />
    </>
  ),
  device: (
    <>
      <rect x="2.75" y="5.75" width="11" height="12.5" rx="1.6" />
      <path d="M6 18.25v2h4.5" />
      <rect x="16.25" y="3.75" width="5" height="16.5" rx="1.4" />
      <path d="M18 5.6h1.5" />
    </>
  ),
  inbox: (
    <>
      <path d="M3.5 13.5l3.3-7.7a1.2 1.2 0 011.1-.8h8.2a1.2 1.2 0 011.1.8l3.3 7.7" />
      <path d="M3.5 13.5h5l1.3 2.3h4.4l1.3-2.3h5v4a1.5 1.5 0 01-1.5 1.5H5a1.5 1.5 0 01-1.5-1.5z" />
    </>
  ),
};

export function Glyph(props: { name: string; size?: number }) {
  const paths = GLYPH_PATHS[props.name];
  if (!paths) return null;
  return (
    <svg
      className="promo-glyph"
      width={props.size ?? 24}
      height={props.size ?? 24}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {paths}
    </svg>
  );
}
