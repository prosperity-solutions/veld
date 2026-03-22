import type { UIMode, Thread } from "../feedback-overlay/types";

export interface Deps {
  setMode: (mode: UIMode) => void;
  toggleToolbar: () => void;
  togglePanel: () => void;
  togglePageComment: () => void;
  hideOverlay: () => void;
  showOverlay: () => void;
  closeActivePopover: () => void;
  addPin: (thread: Thread) => void;
  removePin: (threadId: string) => void;
  renderAllPins: () => void;
  renderPanel: () => void;
  openThreadInPanel: (threadId: string) => void;
  scrollToThread: (threadId: string) => void;
  checkPendingScroll: () => void;
  updateBadge: () => void;
  captureScreenshot: (x: number, y: number, w: number, h: number) => void;
  showCreatePopover: (rect: { x: number; y: number; width: number; height: number }, selector: string | null, tagInfo: string | null, targetEl: Element | null, trace: string[] | null) => void;
  positionTooltip: (el: HTMLElement, viewportRect: DOMRect) => void;
  ensureDrawScript: () => Promise<void>;
}

let _deps: Deps | null = null;

export function registerDeps(d: Deps): void {
  _deps = d;
}

export function deps(): Deps {
  if (!_deps) throw new Error("deps not registered — call registerDeps() in init.ts before using any module");
  return _deps;
}
