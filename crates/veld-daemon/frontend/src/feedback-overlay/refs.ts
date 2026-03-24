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
  toolBtnDraw: HTMLElement;
  toolBtnPageComment: HTMLElement;
  toolBtnComments: HTMLElement;
  toolBtnHide: HTMLElement;
  toolbarOverflow: HTMLElement;
  listeningModule: HTMLElement;

  // Light DOM
  overlay: HTMLElement;
  hoverOutline: HTMLElement;
  componentTraceEl: HTMLElement;
  screenshotRect: HTMLElement;

  // Panel
  panel: HTMLElement;
  panelBody: HTMLElement;
  panelHeadTitle: HTMLElement;
  panelBackBtn: HTMLElement;
  markReadBtn: HTMLElement;
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
    toolBtnDraw: null!,
    toolBtnPageComment: null!,
    toolBtnComments: null!,
    toolBtnHide: null!,
    toolbarOverflow: null!,
    listeningModule: null!,
    overlay: null!,
    hoverOutline: null!,
    componentTraceEl: null!,
    screenshotRect: null!,
    panel: null!,
    panelBody: null!,
    panelHeadTitle: null!,
    panelBackBtn: null!,
    markReadBtn: null!,
    segBtnActive: null!,
    segBtnResolved: null!,
    tooltip: null!,
  };
}
