// DOM scaffolding — builds all the UI elements and attaches them to shadow/light DOM.
import { S } from "./state";
import type { ThemeMode } from "./state";
import { mkEl } from "./helpers";
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
  S.overlay = mkEl("div", "overlay");
  document.body.appendChild(S.overlay);
  initBackdropEvents();

  S.hoverOutline = mkEl("div", "hover-outline");
  document.body.appendChild(S.hoverOutline);

  S.componentTraceEl = mkEl("div", "component-trace");
  document.body.appendChild(S.componentTraceEl);

  // Toolbar container (shadow DOM)
  S.toolbarContainer = mkEl("div", "toolbar-container");
  S.toolbar = mkEl("div", "toolbar");

  S.toolBtnSelect = makeToolBtn("select-element", ICONS.crosshair, tipHtml("Select element", [KEY_MOD, KEY_SHIFT, "F"]));
  S.toolBtnScreenshot = makeToolBtn("screenshot", ICONS.screenshot, tipHtml("Screenshot", [KEY_MOD, KEY_SHIFT, "S"]));
  S.toolBtnDraw = makeToolBtn("draw", ICONS.draw, tipHtml("Draw", [KEY_MOD, KEY_SHIFT, "D"]));
  S.toolBtnPageComment = makeToolBtn("page-comment", ICONS.pageComment, tipHtml("Page comment", [KEY_MOD, KEY_SHIFT, "P"]));
  S.toolBtnComments = makeToolBtn("show-comments", ICONS.chat, tipHtml("Threads", [KEY_MOD, KEY_SHIFT, "C"]));

  S.toolbar.appendChild(S.toolBtnSelect);
  S.toolbar.appendChild(S.toolBtnScreenshot);
  S.toolbar.appendChild(S.toolBtnDraw);
  S.toolbar.appendChild(S.toolBtnPageComment);
  S.toolbar.appendChild(S.toolBtnComments);

  // Listening section
  S.listeningModule = mkEl("div", "listening");
  const listenSep = mkEl("div", "separator");
  S.listeningModule.appendChild(listenSep);
  const listenDot = mkEl("span", "listening-dot");
  attachTooltip(listenDot, "Agent is listening");
  S.listeningModule.appendChild(listenDot);
  const allGoodBtn = mkEl("button", "listening-allgood", "All Good");
  allGoodBtn.addEventListener("click", function (e) { e.stopPropagation(); sendAllGood(); });
  S.listeningModule.appendChild(allGoodBtn);
  S.toolbar.appendChild(S.listeningModule);

  // Separator
  S.toolbar.appendChild(mkEl("div", "separator"));

  // Shortcuts toggle
  const toolBtnShortcuts = mkEl("button", "tool-btn");
  toolBtnShortcuts.innerHTML = ICONS.keyboard;
  attachTooltip(toolBtnShortcuts, tipHtml("Disable shortcuts", []));
  toolBtnShortcuts.addEventListener("click", function (e) {
    e.stopPropagation();
    S.shortcutsDisabled = !S.shortcutsDisabled;
    toolBtnShortcuts.classList.toggle(PREFIX + "tool-active", S.shortcutsDisabled);
    toast(S.shortcutsDisabled ? "Shortcuts disabled" : "Shortcuts enabled");
  });
  S.toolbar.appendChild(toolBtnShortcuts);

  // Theme toggle
  const THEME_ICONS: Record<ThemeMode, string> = {
    auto: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>',
    dark: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z"/></svg>',
    light: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></svg>'
  };
  const THEME_LABELS: Record<ThemeMode, string> = { auto: "Auto (contrast)", dark: "Dark", light: "Light" };
  const THEME_ORDER: ThemeMode[] = ["auto", "dark", "light"];
  const toolBtnTheme = mkEl("button", "tool-btn");
  toolBtnTheme.innerHTML = THEME_ICONS[S.theme];
  attachTooltip(toolBtnTheme, tipHtml(THEME_LABELS[S.theme], []));
  toolBtnTheme.addEventListener("click", function (e) {
    e.stopPropagation();
    const idx = (THEME_ORDER.indexOf(S.theme) + 1) % THEME_ORDER.length;
    S.theme = THEME_ORDER[idx];
    toolBtnTheme.innerHTML = THEME_ICONS[S.theme];
    S.hostEl.setAttribute("data-theme", S.theme);
    document.documentElement.setAttribute("data-veld-theme", S.theme === "auto" ? "" : S.theme);
    toast("Theme: " + THEME_LABELS[S.theme]);
  });
  S.toolbar.appendChild(toolBtnTheme);

  // Dashboard link
  const toolBtnDashboard = mkEl("button", "tool-btn");
  toolBtnDashboard.innerHTML = ICONS.dashboard;
  attachTooltip(toolBtnDashboard, tipHtml("Management UI", []));
  toolBtnDashboard.addEventListener("click", function (e) {
    e.stopPropagation();
    window.open("https://veld.localhost:" + window.location.port, "_blank");
  });
  S.toolbar.appendChild(toolBtnDashboard);

  // Hide
  S.toolBtnHide = makeToolBtn("hide", ICONS.eyeOff, tipHtml("Hide", [KEY_MOD, KEY_SHIFT, "."]));
  S.toolbar.appendChild(S.toolBtnHide);

  // Screenshot rect (light DOM)
  S.screenshotRect = mkEl("div", "screenshot-rect");
  document.body.appendChild(S.screenshotRect);

  S.toolbarContainer.appendChild(S.toolbar);

  // FAB
  S.fab = mkEl("button", "fab");
  attachTooltip(S.fab, tipHtml("Veld Feedback", [KEY_MOD, KEY_SHIFT, "V"]));
  S.fab.innerHTML = ICONS.logo;
  S.fabBadge = mkEl("span", "badge badge-hidden");
  S.fab.appendChild(S.fabBadge);
  S.fab.addEventListener("click", function () {
    if (S.fabWasDragged) { S.fabWasDragged = false; return; }
    toggleToolbar();
  });
  S.toolbarContainer.appendChild(S.fab);

  S.shadow.appendChild(S.toolbarContainer);
  initDrag();

  // Panel (shadow DOM)
  S.panel = mkEl("div", "panel");
  const panelHead = mkEl("div", "panel-head");
  S.panelBackBtn = mkEl("button", "panel-back-btn");
  S.panelBackBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><polyline points="15 18 9 12 15 6"/></svg>';
  S.panelBackBtn.style.display = "none";
  S.panelBackBtn.addEventListener("click", function (e) { e.stopPropagation(); showThreadList(); });
  panelHead.appendChild(S.panelBackBtn);

  S.panelHeadTitle = mkEl("span", "panel-head-title", "Threads");
  panelHead.appendChild(S.panelHeadTitle);

  const segControl = mkEl("div", "segmented");
  S.segBtnActive = mkEl("button", "segmented-btn segmented-btn-active", "Active");
  S.segBtnActive.addEventListener("click", function () { S.panelTab = "active"; renderPanel(); });
  S.segBtnResolved = mkEl("button", "segmented-btn", "Resolved");
  S.segBtnResolved.addEventListener("click", function () { S.panelTab = "resolved"; renderPanel(); });
  segControl.appendChild(S.segBtnActive);
  segControl.appendChild(S.segBtnResolved);
  panelHead.appendChild(segControl);

  S.markReadBtn = mkEl("button", "panel-mark-read");
  S.markReadBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 7 9.5 17 6 13"/><polyline points="22 7 13.5 17"/></svg>';
  S.markReadBtn.title = "Mark all as read";
  S.markReadBtn.style.display = "none";
  S.markReadBtn.addEventListener("click", function (e) { e.stopPropagation(); markAllRead(); });
  panelHead.appendChild(S.markReadBtn);

  const closeBtn = mkEl("button", "panel-close");
  closeBtn.innerHTML = "&times;";
  closeBtn.addEventListener("click", togglePanel);
  panelHead.appendChild(closeBtn);
  S.panel.appendChild(panelHead);

  S.panelBody = mkEl("div", "panel-body");
  S.panel.appendChild(S.panelBody);

  S.shadow.appendChild(S.panel);
}
