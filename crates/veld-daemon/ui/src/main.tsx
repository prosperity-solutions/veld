import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "@mantine/core/styles.css";
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
