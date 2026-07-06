// Toolbar — thin adapter between the Veld feedback overlay and the arc-menu
// engine. It owns the engine instance, translates engine callbacks into store
// dispatches, and preserves the public API that other modules + tests depend on
// (toggleToolbar, positionRadialButtons, makeToolBtn).
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX, ICONS } from "./constants";
import { updateBadge } from "./badge";
import { deps } from "../shared/registry";
import { saveFabPos } from "./fab";
import { createArcMenu, type ArcItem, type ArcMenuHandle } from "./arc-menu";

let arc: ArcMenuHandle | null = null;

/** The live engine handle (null before buildDOM / in unit tests). */
export function getArc(): ArcMenuHandle | null {
  return arc;
}

/** Create a bare icon button. Label/kbd/action are carried by the ArcItem. */
export function makeToolBtn(action: string, iconSvg: string): HTMLElement {
  const btn = mkEl("button", "tool-btn");
  (btn as HTMLElement & { dataset: DOMStringMap }).dataset.action = action;
  btn.innerHTML = iconSvg;
  return btn;
}

/** Perform a standard tool action (modes / panel / page-comment / hide). */
export function handleToolAction(action: string): void {
  if (action === "select-element") {
    deps().setMode(getState().activeMode === "select-element" ? null : "select-element");
  } else if (action === "screenshot") {
    deps().setMode(getState().activeMode === "screenshot" ? null : "screenshot");
  } else if (action === "draw") {
    deps().setMode(getState().activeMode === "draw" ? null : "draw");
  } else if (action === "page-comment") {
    deps().togglePageComment();
  } else if (action === "show-comments") {
    deps().togglePanel();
  } else if (action === "hide") {
    deps().hideOverlay();
  }
}

/**
 * Instantiate the arc-menu engine. Called once from buildDOM with the bubble's
 * icon element and the assembled root item model.
 */
export function initArc(
  bubbleIcon: HTMLElement,
  rootItems: ArcItem[],
  bubbleTooltip?: { label: string; kbd?: string[] },
): void {
  arc = createArcMenu({
    container: refs.toolbarContainer,
    scope: refs.shadow,
    bubble: refs.fab,
    bubbleIcon,
    prefix: PREFIX,
    items: () => rootItems,
    icons: {
      logo: ICONS.logo,
      close: ICONS.cancel,
      back: ICONS.back,
    },
    bubbleTooltip,
    callbacks: {
      onMove: (x, y, committed) => {
        dispatch({ type: "SET_FAB_POS", cx: x, cy: y });
        if (committed) saveFabPos(x, y);
      },
      onOpenChange: (open) => {
        dispatch({ type: "SET_TOOLBAR_OPEN", open });
        dispatch({ type: "SET_OVERFLOW_OPEN", open: false });
        // Closing the menu exits the menu-coupled inspection mode, but leaves
        // full-screen takeovers (screenshot / draw) running.
        if (!open && getState().activeMode === "select-element") {
          deps().setMode(null);
        }
        updateBadge();
      },
      shouldCloseOnOutsideClick: () => !getState().activeMode,
    },
  });
}

/**
 * Toggle the menu open/closed.
 *
 * With the engine present (production) the engine drives open state and fires
 * onOpenChange to update the store. Without an engine (unit tests) we mutate
 * the store directly so the documented state semantics still hold.
 */
export function toggleToolbar(): void {
  if (arc) {
    arc.toggle();
    return;
  }
  const open = !getState().toolbarOpen;
  dispatch({ type: "SET_TOOLBAR_OPEN", open });
  if (!open) dispatch({ type: "SET_OVERFLOW_OPEN", open: false });
  updateBadge();
}

/** Re-sync the arc when the visible item set changes (e.g. listening dot). */
export function positionRadialButtons(): void {
  arc?.reflow();
}
