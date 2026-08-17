import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
// A second terminal font (Fira Code) is declared in `styles.css` against its latin
// subset only — see the @font-face there for why it is not a package import.
import "@mantine/core/styles.css";
import "@mantine/notifications/styles.css";
import "mantine-contextmenu/styles.css";
import "./styles.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { persistInbox } from "./inbox/persist";
import { watchFullScreen, watchZoom } from "./shell";

// Before the first render: the top bar's traffic-light inset is a CSS rule keyed
// off `<body data-fullscreen>`, and a page that boots in full screen would
// otherwise paint one frame with a 90px gutter for buttons macOS is not drawing.
watchFullScreen();
watchZoom();

// Also before the first render, and outside React: `StrictMode` mounts every effect
// twice in development, so a restore-once done in an effect would run twice — harmless
// here (the restore merges) but it is the kind of thing that stops being harmless. The
// store outlives every component anyway, so booting it beside the root is where it
// belongs. A reload is exactly the moment you were not looking, so an inbox that did not
// survive one was failing at its own job.
persistInbox();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
