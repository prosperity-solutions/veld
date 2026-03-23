// DOM scaffolding — builds all the UI elements and attaches them to shadow/light DOM.
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { ThemeMode } from "./store";
import { mkEl, appendGuarded } from "./helpers";
import { PREFIX, ICONS, KEY_MOD, KEY_SHIFT } from "./constants";
import { initTooltip, attachTooltip, tipHtml } from "./tooltip";
import { toast } from "./toast";
import { initBackdropEvents } from "./backdrop";
import { initDrag } from "./fab";
import { toggleToolbar, makeToolBtn } from "./toolbar";
import { togglePanel, showThreadList, renderPanel, markAllRead } from "./panel";
import { sendAllGood } from "./listening";

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

  // Toolbar container (shadow DOM)
  refs.toolbarContainer = mkEl("div", "toolbar-container");
  refs.toolbar = mkEl("div", "toolbar");

  refs.toolBtnSelect = makeToolBtn("select-element", ICONS.crosshair, tipHtml("Select element", [KEY_MOD, KEY_SHIFT, "F"]));
  refs.toolBtnScreenshot = makeToolBtn("screenshot", ICONS.screenshot, tipHtml("Screenshot", [KEY_MOD, KEY_SHIFT, "S"]));
  refs.toolBtnDraw = makeToolBtn("draw", ICONS.draw, tipHtml("Draw", [KEY_MOD, KEY_SHIFT, "D"]));
  refs.toolBtnPageComment = makeToolBtn("page-comment", ICONS.pageComment, tipHtml("Page comment", [KEY_MOD, KEY_SHIFT, "P"]));
  refs.toolBtnComments = makeToolBtn("show-comments", ICONS.chat, tipHtml("Threads", [KEY_MOD, KEY_SHIFT, "C"]));

  refs.toolbar.appendChild(refs.toolBtnSelect);
  refs.toolbar.appendChild(refs.toolBtnScreenshot);
  refs.toolbar.appendChild(refs.toolBtnDraw);
  refs.toolbar.appendChild(refs.toolBtnPageComment);
  refs.toolbar.appendChild(refs.toolBtnComments);

  // Listening section
  refs.listeningModule = mkEl("div", "listening");
  const listenSep = mkEl("div", "separator");
  refs.listeningModule.appendChild(listenSep);
  const listenDot = mkEl("span", "listening-dot");
  attachTooltip(listenDot, "Agent is listening");
  refs.listeningModule.appendChild(listenDot);
  const allGoodBtn = mkEl("button", "listening-allgood", "All Good");
  allGoodBtn.addEventListener("click", function (e) { e.stopPropagation(); sendAllGood(); });
  refs.listeningModule.appendChild(allGoodBtn);
  refs.toolbar.appendChild(refs.listeningModule);

  // Separator
  refs.toolbar.appendChild(mkEl("div", "separator"));

  // Shortcuts toggle
  const toolBtnShortcuts = mkEl("button", "tool-btn");
  toolBtnShortcuts.innerHTML = ICONS.keyboard;
  attachTooltip(toolBtnShortcuts, tipHtml("Disable shortcuts", []));
  toolBtnShortcuts.addEventListener("click", function (e) {
    e.stopPropagation();
    dispatch({ type: "SET_SHORTCUTS_DISABLED", disabled: !getState().shortcutsDisabled });
    toolBtnShortcuts.classList.toggle(PREFIX + "tool-active", getState().shortcutsDisabled);
    toast(getState().shortcutsDisabled ? "Shortcuts disabled" : "Shortcuts enabled");
  });
  refs.toolbar.appendChild(toolBtnShortcuts);

  // Theme toggle
  const THEME_ICONS: Record<ThemeMode, string> = {
    auto: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>',
    dark: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z"/></svg>',
    light: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></svg>'
  };
  const THEME_LABELS: Record<ThemeMode, string> = { auto: "Auto (contrast)", dark: "Dark", light: "Light" };
  const THEME_ORDER: ThemeMode[] = ["auto", "dark", "light"];
  const toolBtnTheme = mkEl("button", "tool-btn");
  toolBtnTheme.innerHTML = THEME_ICONS[getState().theme];
  attachTooltip(toolBtnTheme, tipHtml(THEME_LABELS[getState().theme], []));
  toolBtnTheme.addEventListener("click", function (e) {
    e.stopPropagation();
    const idx = (THEME_ORDER.indexOf(getState().theme) + 1) % THEME_ORDER.length;
    dispatch({ type: "SET_THEME", theme: THEME_ORDER[idx] });
    toolBtnTheme.innerHTML = THEME_ICONS[getState().theme];
    refs.hostEl.setAttribute("data-theme", getState().theme);
    document.documentElement.setAttribute("data-veld-theme", getState().theme === "auto" ? "" : getState().theme);
    toast("Theme: " + THEME_LABELS[getState().theme]);
  });
  refs.toolbar.appendChild(toolBtnTheme);

  // Dashboard link
  const toolBtnDashboard = mkEl("button", "tool-btn");
  toolBtnDashboard.innerHTML = ICONS.dashboard;
  attachTooltip(toolBtnDashboard, tipHtml("Management UI", []));
  toolBtnDashboard.addEventListener("click", function (e) {
    e.stopPropagation();
    window.open("https://veld.localhost:" + window.location.port, "_blank");
  });
  refs.toolbar.appendChild(toolBtnDashboard);

  // Hide
  refs.toolBtnHide = makeToolBtn("hide", ICONS.eyeOff, tipHtml("Hide", [KEY_MOD, KEY_SHIFT, "."]));
  refs.toolbar.appendChild(refs.toolBtnHide);

  // Screenshot rect (light DOM)
  refs.screenshotRect = mkEl("div", "screenshot-rect");
  appendGuarded(document.body, refs.screenshotRect);

  refs.toolbarContainer.appendChild(refs.toolbar);

  // FAB
  refs.fab = mkEl("button", "fab");
  attachTooltip(refs.fab, tipHtml("Veld Feedback", [KEY_MOD, KEY_SHIFT, "V"]));
  refs.fab.innerHTML = ICONS.logo;
  refs.fabBadge = mkEl("span", "badge badge-hidden");
  refs.fab.appendChild(refs.fabBadge);
  refs.fab.addEventListener("click", function () {
    if (getState().fabWasDragged) { dispatch({ type: "SET_FAB_DRAGGED", dragged: false }); return; }
    toggleToolbar();
  });
  refs.toolbarContainer.appendChild(refs.fab);

  refs.shadow.appendChild(refs.toolbarContainer);
  initDrag();

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
  refs.markReadBtn.title = "Mark all as read";
  refs.markReadBtn.style.display = "none";
  refs.markReadBtn.addEventListener("click", function (e) { e.stopPropagation(); markAllRead(); });
  panelHead.appendChild(refs.markReadBtn);

  const closeBtn = mkEl("button", "panel-close");
  closeBtn.innerHTML = "&times;";
  closeBtn.addEventListener("click", togglePanel);
  panelHead.appendChild(closeBtn);
  refs.panel.appendChild(panelHead);

  refs.panelBody = mkEl("div", "panel-body");
  refs.panel.appendChild(refs.panelBody);

  refs.shadow.appendChild(refs.panel);
}
