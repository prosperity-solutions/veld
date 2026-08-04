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

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
