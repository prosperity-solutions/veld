// DOM scaffolding — builds all the UI elements and attaches them to shadow/light DOM.
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { ThemeMode } from "./store";
import { mkEl, appendGuarded } from "./helpers";
import { PREFIX, ICONS, KEY_MOD, KEY_SHIFT } from "./constants";
import { initTooltip, attachTooltip } from "./tooltip";
import { toast } from "./toast";
import { initBackdropEvents } from "./backdrop";
import { initArc, makeToolBtn, handleToolAction } from "./toolbar";
import type { ArcItem } from "./arc-menu";
import { togglePanel, togglePanelSide, showThreadList, renderPanel, markAllRead, applyPanelLayout, togglePanelMode, initPanelResize } from "./panel";
import { sendAllGood } from "./listening";
import { captureFullScreenshot } from "./screenshot";

export function buildDOM(): void {
  initTooltip();

  // Light DOM elements
  refs.overlay = mkEl("div", "overlay");
  appendGuarded(document.body, refs.overlay);
  initBackdropEvents();

  refs.hoverOutline = mkEl("div", "hover-outline");
  appendGuarded(document.body, refs.hoverOutline);

  refs.componentTraceEl = mkEl("div", "component-trace");
  appendGuarded(document.body, refs.componentTraceEl);

  // Screenshot selection rectangle (light DOM) — drawn on the backdrop.
  // Four corner brackets give it the "viewfinder" look asked for instead of
  // a bare dashed box; the huge box-shadow (see CSS) is the same spotlight
  // trick as the hover-outline, dimming everything outside the selection.
  refs.screenshotRect = mkEl("div", "screenshot-rect");
  (["tl", "tr", "bl", "br"] as const).forEach((corner) => {
    refs.screenshotRect.appendChild(mkEl("span", "screenshot-corner screenshot-corner-" + corner));
  });
  appendGuarded(document.body, refs.screenshotRect);

  // Screenshot mode instruction banner (light DOM) — explicit, always-visible
  // guidance instead of a single toast that scrolls off. Doubles as the
  // "capture everything, no cropping" escape hatch.
  refs.screenshotBanner = mkEl("div", "screenshot-banner");
  refs.screenshotBanner.appendChild(
    mkEl("span", "screenshot-banner-text", "Drag to select an area to capture"),
  );
  refs.screenshotFullBtn = mkEl("button", "screenshot-banner-btn", "Capture full screen");
  refs.screenshotFullBtn.addEventListener("click", function (e) {
    e.preventDefault();
    e.stopPropagation();
    captureFullScreenshot();
  });
  refs.screenshotBanner.appendChild(refs.screenshotFullBtn);
  refs.screenshotBanner.appendChild(mkEl("span", "screenshot-banner-hint", "Esc to cancel"));
  appendGuarded(document.body, refs.screenshotBanner);

  // Float container (shadow DOM) — anchor for the arc-menu engine. Zero-size,
  // translated to the bubble center; the engine builds its goo/glow/icon layers
  // inside it.
  refs.toolbarContainer = mkEl("div", "toolbar-container");
  refs.toolbar = refs.toolbarContainer; // alias for compatibility

  // --- Primary tool icons (reused by the engine as crisp icon overlays) ---
  refs.toolBtnSelect = makeToolBtn("select-element", ICONS.crosshair);
  refs.toolBtnScreenshot = makeToolBtn("screenshot", ICONS.screenshot);
  refs.toolBtnPageComment = makeToolBtn("page-comment", ICONS.pageComment);
  refs.toolBtnComments = makeToolBtn("show-comments", ICONS.chat);

  // Listening dot — conditionally-visible tool.
  refs.listeningModule = mkEl("button", "tool-btn listening-dot");
  refs.listeningModule.innerHTML = ICONS.check;

  // Three-dot overflow — opens the secondary tools as a submenu.
  refs.moreBtn = makeToolBtn("more", ICONS.more);

  // --- Overflow (secondary) tools ---
  const toolBtnShortcuts = makeToolBtn("shortcuts", ICONS.keyboard);
  const THEME_ICONS: Record<ThemeMode, string> = {
    auto: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>',
    dark: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z"/></svg>',
    light: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></svg>'
  };
  const THEME_LABELS: Record<ThemeMode, string> = { auto: "Auto (contrast)", dark: "Dark", light: "Light" };
  const THEME_ORDER: ThemeMode[] = ["auto", "dark", "light"];
  const toolBtnTheme = makeToolBtn("theme", THEME_ICONS[getState().theme]);
  const toolBtnDashboard = makeToolBtn("dashboard", ICONS.dashboard);
  refs.toolBtnHide = makeToolBtn("hide", ICONS.eyeOff);

  // Reflect the current theme on the icon + host, and persist it.
  function applyTheme(): void {
    const theme = getState().theme;
    toolBtnTheme.innerHTML = THEME_ICONS[theme];
    refs.hostEl.setAttribute("data-theme", theme);
    document.documentElement.setAttribute("data-veld-theme", theme === "auto" ? "" : theme);
    try { localStorage.setItem("veld-theme", theme); } catch (_) { /* ignore */ }
  }

  // Keep the ref arrays populated (legacy compat / debugging).
  refs.radialButtons = [
    refs.toolBtnSelect,
    refs.toolBtnScreenshot,
    refs.toolBtnPageComment,
    refs.toolBtnComments,
    refs.listeningModule,
    refs.moreBtn,
  ];
  refs.overflowButtons = [toolBtnShortcuts, toolBtnTheme, toolBtnDashboard, refs.toolBtnHide];
  refs.toolbarOverflow = refs.toolbarContainer; // test compat

  // --- Item model ---------------------------------------------------------
  const overflowItems: ArcItem[] = [
    {
      id: "shortcuts",
      el: toolBtnShortcuts,
      label: "Shortcuts",
      stayOpen: true,
      isActive: () => getState().shortcutsDisabled,
      onSelect: () => {
        dispatch({ type: "SET_SHORTCUTS_DISABLED", disabled: !getState().shortcutsDisabled });
        toolBtnShortcuts.classList.toggle(PREFIX + "tool-active", getState().shortcutsDisabled);
        toast(getState().shortcutsDisabled ? "Shortcuts disabled" : "Shortcuts enabled");
      },
    },
    {
      id: "theme",
      el: toolBtnTheme,
      label: "Theme",
      stayOpen: true,
      onSelect: () => {
        const idx = (THEME_ORDER.indexOf(getState().theme) + 1) % THEME_ORDER.length;
        dispatch({ type: "SET_THEME", theme: THEME_ORDER[idx] });
        applyTheme();
        toast("Theme: " + THEME_LABELS[getState().theme]);
      },
    },
    {
      id: "dashboard",
      el: toolBtnDashboard,
      label: "Management UI",
      onSelect: () => window.open("https://veld.localhost:" + window.location.port, "_blank"),
    },
    {
      id: "hide",
      el: refs.toolBtnHide,
      label: "Hide",
      kbd: [KEY_MOD, KEY_SHIFT, "."],
      onSelect: () => handleToolAction("hide"),
    },
  ];

  const rootItems: ArcItem[] = [
    {
      id: "select-element",
      el: refs.toolBtnSelect,
      label: "Select element",
      kbd: [KEY_MOD, KEY_SHIFT, "F"],
      stayOpen: true,
      isActive: () => getState().activeMode === "select-element",
      onSelect: () => handleToolAction("select-element"),
    },
    {
      id: "screenshot",
      el: refs.toolBtnScreenshot,
      label: "Screenshot",
      kbd: [KEY_MOD, KEY_SHIFT, "S"],
      isActive: () => getState().activeMode === "screenshot",
      onSelect: () => handleToolAction("screenshot"),
    },
    {
      id: "page-comment",
      el: refs.toolBtnPageComment,
      label: "Page comment",
      kbd: [KEY_MOD, KEY_SHIFT, "P"],
      onSelect: () => handleToolAction("page-comment"),
    },
    {
      id: "show-comments",
      el: refs.toolBtnComments,
      label: "Threads",
      kbd: [KEY_MOD, KEY_SHIFT, "C"],
      onSelect: () => handleToolAction("show-comments"),
    },
    {
      id: "listening",
      el: refs.listeningModule,
      label: "Done — no more feedback",
      isVisible: () => getState().agentListening,
      onSelect: () => sendAllGood(),
    },
    {
      id: "more",
      el: refs.moreBtn,
      label: "More",
      sub: overflowItems,
    },
  ];

  // --- Bubble -------------------------------------------------------------
  refs.fab = mkEl("button", "fab");
  const fabIcon = mkEl("div", "fab-icon");
  refs.fab.appendChild(fabIcon);
  refs.fabBadge = mkEl("span", "badge badge-hidden");
  refs.fab.appendChild(refs.fabBadge);

  refs.shadow.appendChild(refs.toolbarContainer);

  // Boot the arc-menu engine (it moves the bubble into its icon layer).
  initArc(fabIcon, rootItems, { label: "Veld Feedback", kbd: [KEY_MOD, KEY_SHIFT, "V"] });

  // Apply the restored theme (icon + host attributes).
  applyTheme();

  // Panel (shadow DOM)
  refs.panel = mkEl("div", "panel");
  const panelHead = mkEl("div", "panel-head");
  refs.panelBackBtn = mkEl("button", "panel-back-btn");
  refs.panelBackBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><polyline points="15 18 9 12 15 6"/></svg>';
  refs.panelBackBtn.style.display = "none";
  refs.panelBackBtn.addEventListener("click", function (e) { e.stopPropagation(); showThreadList(); });
  panelHead.appendChild(refs.panelBackBtn);

  refs.panelHeadTitle = mkEl("span", "panel-head-title", "Threads");
  panelHead.appendChild(refs.panelHeadTitle);

  const segControl = mkEl("div", "segmented");
  refs.segBtnActive = mkEl("button", "segmented-btn segmented-btn-active", "Active");
  refs.segBtnActive.addEventListener("click", function () { dispatch({ type: "SET_PANEL_TAB", tab: "active" }); renderPanel(); });
  refs.segBtnResolved = mkEl("button", "segmented-btn", "Resolved");
  refs.segBtnResolved.addEventListener("click", function () { dispatch({ type: "SET_PANEL_TAB", tab: "resolved" }); renderPanel(); });
  segControl.appendChild(refs.segBtnActive);
  segControl.appendChild(refs.segBtnResolved);
  panelHead.appendChild(segControl);

  refs.markReadBtn = mkEl("button", "panel-mark-read");
  refs.markReadBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 7 9.5 17 6 13"/><polyline points="22 7 13.5 17"/></svg>';
  attachTooltip(refs.markReadBtn, "Mark all as read");
  refs.markReadBtn.style.display = "none";
  refs.markReadBtn.addEventListener("click", function (e) { e.stopPropagation(); markAllRead(); });
  panelHead.appendChild(refs.markReadBtn);

  const sideBtn = mkEl("button", "panel-side-toggle");
  sideBtn.innerHTML = ICONS.panelSide;
  attachTooltip(sideBtn, "Switch panel side");
  sideBtn.addEventListener("click", function (e) { e.stopPropagation(); togglePanelSide(); });
  panelHead.appendChild(sideBtn);

  refs.panelModeBtn = mkEl("button", "panel-mode-toggle");
  refs.panelModeBtn.innerHTML = ICONS.dock;
  attachTooltip(refs.panelModeBtn, "Dock / float panel");
  refs.panelModeBtn.addEventListener("click", function (e) { e.stopPropagation(); togglePanelMode(); });
  panelHead.appendChild(refs.panelModeBtn);

  const closeBtn = mkEl("button", "panel-close");
  closeBtn.innerHTML = "&times;";
  closeBtn.addEventListener("click", togglePanel);
  panelHead.appendChild(closeBtn);
  refs.panel.appendChild(panelHead);

  refs.panelBody = mkEl("div", "panel-body");
  refs.panel.appendChild(refs.panelBody);

  refs.panelResize = mkEl("div", "panel-resize");
  refs.panel.appendChild(refs.panelResize);
  initPanelResize();
  applyPanelLayout();

  refs.shadow.appendChild(refs.panel);
}
