import { createTheme, type MantineColorsTuple } from "@mantine/core";

// Mantine theme mapped onto the Veld Desktop design-handoff tokens (see
// src/styles.css — those CSS variables stay the source of truth for the
// custom components; this mapping keeps Mantine-rendered chrome on the same
// palette).

// Handoff accent green (oklch(0.74 0.14 158) ≈ #3fbf7f) as a 10-shade tuple.
const green: MantineColorsTuple = [
  "#e3f8ec",
  "#c2eed6",
  "#9fe4c0",
  "#7cd9a9",
  "#5ccf94",
  "#3fbf7f", // ≈ --accent (dark scheme)
  "#35a76e",
  "#2a8f5d", // ≈ --accent (light scheme, needs contrast on white)
  "#20774c",
  "#155f3b",
];

// Dark surface ramp from the handoff: bg/panel/panel2/elev/borders/text.
const dark: MantineColorsTuple = [
  "#e7e9ec", // --text
  "#c3c7cc",
  "#98a0a9", // --muted
  "#666d76", // --faint
  "#363b43", // --border2
  "#2a2e35", // --border
  "#1a1d21", // --panel2
  "#141619", // --panel (Mantine body in dark scheme)
  "#0d0e10", // --bg
  "#08090b",
];

export const theme = createTheme({
  fontFamily: '"Inter Variable", system-ui, sans-serif',
  fontFamilyMonospace: '"JetBrains Mono Variable", ui-monospace, monospace',
  primaryColor: "green",
  primaryShade: { light: 7, dark: 5 },
  colors: { green, dark },
  defaultRadius: "md",
  headings: { fontWeight: "600" },
  components: {
    Tooltip: {
      defaultProps: { openDelay: 400 },
    },
  },
});
