// ---------------------------------------------------------------------------
// Veld Feedback Overlay — entry point
// Creates the <veld-feedback> custom element with Shadow DOM,
// injects CSS, initializes state, and starts the overlay.
// ---------------------------------------------------------------------------

import { SHADOW_CSS, LIGHT_CSS } from "./styles";
import { initState } from "./state";
import { init } from "./init";

if (!window.__veld_feedback_initialised) {
  window.__veld_feedback_initialised = true;

  // Create host element with shadow DOM
  const hostEl = document.createElement("veld-feedback");
  hostEl.style.cssText = "display:contents";
  document.body.appendChild(hostEl);

  const shadow = hostEl.attachShadow({ mode: "open" });

  // Inject shadow DOM CSS (all visual styles, fully encapsulated)
  const shadowStyle = document.createElement("style");
  shadowStyle.textContent = SHADOW_CSS;
  shadow.appendChild(shadowStyle);

  // Inject light DOM CSS (minimal, for elements that must live in document.body)
  const lightStyle = document.createElement("style");
  lightStyle.textContent = LIGHT_CSS;
  lightStyle.setAttribute("data-veld", "light");
  (document.head || document.documentElement).appendChild(lightStyle);

  // Initialize shared state
  initState(shadow, hostEl);

  // Start the overlay
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
}
