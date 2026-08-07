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
import { watchFullScreen } from "./shell";

// Before the first render: the top bar's traffic-light inset is a CSS rule keyed
// off `<body data-fullscreen>`, and a page that boots in full screen would
// otherwise paint one frame with a 90px gutter for buttons macOS is not drawing.
watchFullScreen();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
