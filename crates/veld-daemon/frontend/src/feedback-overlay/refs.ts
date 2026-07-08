/** Static DOM references — set once during buildDOM(), never mutated after. */
export interface DOMRefs {
  shadow: ShadowRoot;
  hostEl: HTMLElement;

  // Toolbar
  toolbarContainer: HTMLElement;
  fab: HTMLElement;
  fabBadge: HTMLElement;
  toolbar: HTMLElement;
  toolBtnSelect: HTMLElement;
  toolBtnScreenshot: HTMLElement;
  toolBtnPageComment: HTMLElement;
  toolBtnComments: HTMLElement;
  toolBtnHide: HTMLElement;
  toolbarOverflow: HTMLElement;
  listeningModule: HTMLElement;
  /** Primary radial buttons in display order. */
  radialButtons: HTMLElement[];
  /** Secondary (overflow) radial buttons in display order. */
  overflowButtons: HTMLElement[];
  /** The three-dot overflow toggle button. */
  moreBtn: HTMLElement;

  // Light DOM
  overlay: HTMLElement;
  hoverOutline: HTMLElement;
  componentTraceEl: HTMLElement;
  screenshotRect: HTMLElement;
  screenshotBanner: HTMLElement;
  screenshotFullBtn: HTMLElement;

  // Panel
  panel: HTMLElement;
  panelBody: HTMLElement;
  panelHeadTitle: HTMLElement;
  panelBackBtn: HTMLElement;
  markReadBtn: HTMLElement;
  panelModeBtn: HTMLElement;
  panelResize: HTMLElement;
  segBtnActive: HTMLElement;
  segBtnResolved: HTMLElement;

  // Tooltip
  tooltip: HTMLElement;
}

// eslint-disable-next-line prefer-const
export let refs: DOMRefs;

export function initRefs(shadow: ShadowRoot, hostEl: HTMLElement): void {
  refs = {
    shadow,
    hostEl,
    toolbarContainer: null!,
    fab: null!,
    fabBadge: null!,
    toolbar: null!,
    toolBtnSelect: null!,
    toolBtnScreenshot: null!,
    toolBtnPageComment: null!,
    toolBtnComments: null!,
    toolBtnHide: null!,
    toolbarOverflow: null!,
    listeningModule: null!,
    radialButtons: [],
    overflowButtons: [],
    moreBtn: null!,
    overlay: null!,
    hoverOutline: null!,
    componentTraceEl: null!,
    screenshotRect: null!,
    screenshotBanner: null!,
    screenshotFullBtn: null!,
    panel: null!,
    panelBody: null!,
    panelHeadTitle: null!,
    panelBackBtn: null!,
    markReadBtn: null!,
    panelModeBtn: null!,
    panelResize: null!,
    segBtnActive: null!,
    segBtnResolved: null!,
    tooltip: null!,
  };
}
