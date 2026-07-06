import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl, findThread, hasUnread, timeAgo, getThreadPageUrl, submitOnModEnter } from "./helpers";
import { PREFIX, ICONS, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { updateBadge } from "./badge";
import type { Thread, Message } from "./types";
import { deps } from "../shared/registry";

export function togglePanel(): void {
  dispatch({ type: "SET_PANEL_OPEN", open: !getState().panelOpen });
  if (getState().panelOpen) dispatch({ type: "SET_EXPANDED_THREAD", threadId: null });
  refs.panel.classList.toggle(PREFIX + "panel-open", getState().panelOpen);
  applyPanelLayout();
  if (getState().panelOpen) renderPanel();
}

export function togglePanelSide(): void {
  const newSide = getState().panelSide === "right" ? "left" : "right";
  dispatch({ type: "SET_PANEL_SIDE", side: newSide });
  syncPanelSideClass();
  try { localStorage.setItem("veld-panel-side", newSide); } catch (_) {}
}

export function syncPanelSideClass(): void {
  refs.panel.classList.toggle(PREFIX + "panel-left", getState().panelSide === "left");
  applyPanelLayout();
}

/** Apply the current panel width and float/dock layout.
 *
 *  In dock mode we shrink the document via a margin on <html> so the page
 *  reflows beside the panel; in float mode the panel overlays the page. The
 *  margin is cleared when the panel is closed or the overlay is hidden.
 *
 *  Caveat: `position: fixed`/`sticky` page elements are viewport-relative and
 *  won't respect the margin — an inherent limit of pushing pages we don't own,
 *  so float stays the default. */
export function applyPanelLayout(): void {
  const s = getState();
  if (refs.panel) refs.panel.style.width = s.panelWidth + "px";
  if (refs.panelModeBtn) {
    refs.panelModeBtn.classList.toggle(PREFIX + "panel-mode-toggle-active", s.panelMode === "dock");
  }
  const root = document.documentElement;
  root.style.marginLeft = "";
  root.style.marginRight = "";
  if (s.panelMode === "dock" && s.panelOpen && !s.hidden) {
    root.style.transition = "margin .2s ease";
    if (s.panelSide === "left") root.style.marginLeft = s.panelWidth + "px";
    else root.style.marginRight = s.panelWidth + "px";
  }
}

/** Toggle between float (overlay) and dock (push content aside) modes. */
export function togglePanelMode(): void {
  const mode = getState().panelMode === "dock" ? "float" : "dock";
  dispatch({ type: "SET_PANEL_MODE", mode });
  try { localStorage.setItem("veld-panel-mode", mode); } catch (_) { /* ignore */ }
  applyPanelLayout();
  toast(mode === "dock" ? "Panel docked — content pushed aside" : "Panel floating over content");
}

/** Wire drag-to-resize on the panel's inner edge; persists the chosen width. */
export function initPanelResize(): void {
  const handle = refs.panelResize;
  if (!handle) return;
  const MIN = 300;
  const maxW = function () { return Math.min(760, Math.round(window.innerWidth * 0.9)); };
  let dragging = false;
  const onMove = function (e: PointerEvent) {
    if (!dragging) return;
    const raw = getState().panelSide === "left" ? e.clientX : window.innerWidth - e.clientX;
    const w = Math.max(MIN, Math.min(maxW(), Math.round(raw)));
    dispatch({ type: "SET_PANEL_WIDTH", width: w });
    applyPanelLayout();
  };
  const onUp = function () {
    if (!dragging) return;
    dragging = false;
    document.removeEventListener("pointermove", onMove, true);
    document.removeEventListener("pointerup", onUp, true);
    try { localStorage.setItem("veld-panel-width", String(getState().panelWidth)); } catch (_) { /* ignore */ }
  };
  handle.addEventListener("pointerdown", function (e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    dragging = true;
    document.addEventListener("pointermove", onMove, true);
    document.addEventListener("pointerup", onUp, true);
  });
}

export function showThreadDetail(threadId: string): void {
  dispatch({ type: "SET_EXPANDED_THREAD", threadId });
  renderPanel();
}

export function showThreadList(): void {
  dispatch({ type: "SET_EXPANDED_THREAD", threadId: null });
  renderPanel();
}

export function openThreadInPanel(threadId: string): void {
  dispatch({ type: "SET_PANEL_OPEN", open: true });
  dispatch({ type: "SET_PANEL_TAB", tab: "active" });
  dispatch({ type: "SET_EXPANDED_THREAD", threadId });
  refs.panel.classList.add(PREFIX + "panel-open");
  applyPanelLayout();
  renderPanel();
}

function updateSegmentedControl(): void {
  if (refs.segBtnActive && refs.segBtnResolved) {
    const activeCount = getState().threads.filter(function (t: Thread) { return t.status === "open"; }).length;
    const resolvedCount = getState().threads.filter(function (t: Thread) { return t.status === "resolved"; }).length;
    refs.segBtnActive.textContent = "Active" + (activeCount ? " (" + activeCount + ")" : "");
    refs.segBtnResolved.textContent = "Resolved" + (resolvedCount ? " (" + resolvedCount + ")" : "");
    refs.segBtnActive.className = PREFIX + "segmented-btn" + (getState().panelTab === "active" ? " " + PREFIX + "segmented-btn-active" : "");
    refs.segBtnResolved.className = PREFIX + "segmented-btn" + (getState().panelTab === "resolved" ? " " + PREFIX + "segmented-btn-active" : "");
  }
}

export function updateMarkReadBtn(): void {
  if (!refs.markReadBtn) return;
  // Hide in thread detail view — only show in list view
  if (getState().expandedThreadId) {
    refs.markReadBtn.style.display = "none";
    return;
  }
  const anyUnread = getState().threads.some(function (t: Thread) { return hasUnread(t); });
  refs.markReadBtn.style.display = anyUnread ? "" : "none";
}

export function renderPanel(): void {
  refs.panelBody.innerHTML = "";

  const expandedId = getState().expandedThreadId;
  if (expandedId) {
    const thread = findThread(getState().threads, expandedId);
    if (thread) {
      refs.panelBackBtn.style.display = "";
      // Hide segmented control in detail view
      const segControl = refs.panelBackBtn.parentElement?.querySelector("." + PREFIX + "segmented") as HTMLElement | null;
      if (segControl) segControl.style.display = "none";
      if (refs.markReadBtn) refs.markReadBtn.style.display = "none";
      refs.panelHeadTitle.textContent = "Thread";
      refs.panelBody.classList.toggle(PREFIX + "panel-body-thread", thread.status === "open");
      renderThreadDetail(thread);
      return;
    }
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: null });
  }

  refs.panelBody.classList.remove(PREFIX + "panel-body-thread");
  refs.panelBackBtn.style.display = "none";
  const segControl = refs.panelBackBtn.parentElement?.querySelector("." + PREFIX + "segmented") as HTMLElement | null;
  if (segControl) segControl.style.display = "";
  refs.panelHeadTitle.textContent = "Threads";
  updateSegmentedControl();
  updateMarkReadBtn();

  if (getState().panelTab === "active") {
    renderActiveThreads();
  } else {
    renderResolvedThreads();
  }
}

const COPY_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>';

function makeCopyRow(label: string, displayValue: string, cls: string, copyValue?: string): HTMLElement {
  const row = mkEl("div", cls);
  row.appendChild(document.createTextNode(label + displayValue));
  const icon = mkEl("span", "panel-detail-copy-icon");
  icon.innerHTML = COPY_SVG;
  row.appendChild(icon);
  const valueToCopy = copyValue !== undefined ? copyValue : displayValue;
  row.addEventListener("click", function (e) {
    e.stopPropagation();
    navigator.clipboard.writeText(valueToCopy).then(function () {
      icon.innerHTML = ICONS.check;
      setTimeout(function () { icon.innerHTML = COPY_SVG; }, 1500);
    });
  });
  return row;
}

function renderThreadDetail(thread: Thread): void {
  const header = mkEl("div", "panel-detail-header");
  header.appendChild(makeCopyRow("ID: ", thread.id.substring(0, 20) + "\u2026", "panel-detail-id", thread.id));

  const pageUrl = getThreadPageUrl(thread);
  let titleText: string;
  if (thread.scope.type === "global") {
    titleText = "Global";
  } else if (thread.scope.type === "page") {
    titleText = "Page " + pageUrl;
    if (pageUrl === "/") titleText += " (home)";
  } else {
    titleText = "Page " + (pageUrl || "?");
    if (pageUrl === "/") titleText += " (home)";
  }
  header.appendChild(mkEl("div", "panel-detail-title", titleText));

  const onDifferentPage = pageUrl && pageUrl !== window.location.pathname;
  const hasScrollTarget = thread.scope.type === "element";
  if (hasScrollTarget || onDifferentPage) {
    const goLabel = onDifferentPage ? "Go to page \u2192" : "Go to comment \u2192";
    const goLink = mkEl("a", "panel-detail-page-link", goLabel) as HTMLAnchorElement;
    goLink.href = pageUrl || "#";
    goLink.addEventListener("click", function (e) {
      e.preventDefault();
      deps().scrollToThread(thread.id);
    });
    header.appendChild(goLink);
  }

  if (thread.component_trace && thread.component_trace.length) {
    header.appendChild(makeCopyRow("", thread.component_trace.join(" > "), "panel-detail-trace"));
  }
  if (thread.scope.type === "element" && thread.scope.selector) {
    header.appendChild(makeCopyRow("", thread.scope.selector, "panel-detail-selector"));
  }

  refs.panelBody.appendChild(header);

  if (thread.status === "resolved") {
    const msgList = mkEl("div", "thread-messages-list");
    thread.messages.forEach(function (msg: Message) {
      const msgEl = mkEl("div", "message message-" + msg.author);
      const icon = mkEl("span", "message-author-icon");
      icon.innerHTML = msg.author === "agent" ? ICONS.robot : ICONS.chat;
      msgEl.appendChild(icon);
      const body = mkEl("div", "message-body");
      body.appendChild(mkEl("div", "message-text", msg.body));
      const authorLabel = msg.author === "agent" ? "Agent" : "You";
      body.appendChild(mkEl("div", "message-meta", authorLabel + " \u00B7 " + timeAgo(msg.created_at)));
      msgEl.appendChild(body);
      msgList.appendChild(msgEl);
    });
    refs.panelBody.appendChild(msgList);

    const reopenRow = mkEl("div", "thread-input-actions");
    const reopenBtn = mkEl("button", "btn btn-primary btn-sm", "Reopen Thread");
    reopenBtn.addEventListener("click", function () {
      api("POST", "/threads/" + thread.id + "/reopen").then(function () {
        thread.status = "open";
        dispatch({ type: "SET_THREADS", threads: [...getState().threads] });
        showThreadList();
        deps().renderAllPins();
        toast("Thread reopened");
      });
    });
    reopenRow.appendChild(reopenBtn);
    refs.panelBody.appendChild(reopenRow);
  } else {
    refs.panelBody.appendChild(renderThreadMessages(thread));
  }
}

function lastMessageAuthor(t: Thread): "human" | "agent" | undefined {
  return t.messages.length ? t.messages[t.messages.length - 1].author : undefined;
}

function renderActiveThreads(): void {
  const active = getState().threads.filter(function (t: Thread) { return t.status === "open"; });
  const byRecency = function (a: Thread, b: Thread) {
    return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
  };
  // Two lanes mirroring the agent's queue: a thread whose last message is the
  // agent's is waiting on you ("Your turn"); one whose last message is human
  // (or none yet) is in the agent's queue ("With the agent"). Both lanes always
  // render — with a count and an empty state when there's nothing in them.
  const yourTurn = active.filter(function (t) { return lastMessageAuthor(t) === "agent"; }).sort(byRecency);
  const withAgent = active.filter(function (t) { return lastMessageAuthor(t) !== "agent"; }).sort(byRecency);
  renderLane("Your turn", ICONS.person, yourTurn, "Nothing needs your reply.");
  renderLane("With the agent", ICONS.robot, withAgent, "Nothing waiting on the agent.");
}

function renderLane(label: string, icon: string, threads: Thread[], emptyText: string): void {
  const section = mkEl("div", "panel-section");
  const heading = mkEl("div", "panel-section-heading");
  const iconEl = mkEl("span", "panel-section-icon");
  iconEl.innerHTML = icon;
  heading.appendChild(iconEl);
  heading.appendChild(document.createTextNode(label + " (" + threads.length + ")"));
  section.appendChild(heading);
  if (threads.length) {
    threads.forEach(function (t: Thread) { section.appendChild(makeThreadCard(t, false)); });
  } else {
    section.appendChild(mkEl("div", "panel-lane-empty", emptyText));
  }
  refs.panelBody.appendChild(section);
}

function renderResolvedThreads(): void {
  const resolved = getState().threads.filter(function (t: Thread) { return t.status === "resolved"; });
  if (!resolved.length) {
    refs.panelBody.appendChild(mkEl("div", "panel-empty", "No resolved threads."));
    return;
  }
  resolved.sort(function (a: Thread, b: Thread) {
    return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
  });
  resolved.forEach(function (t: Thread) { refs.panelBody.appendChild(makeThreadCard(t, true)); });
}

function makeThreadCard(thread: Thread, isResolved: boolean): HTMLElement {
  const card = mkEl("div", "thread-card" + (isResolved ? " thread-card-resolved" : ""));
  if (hasUnread(thread) && !isResolved) card.classList.add(PREFIX + "thread-card-unread");
  (card as HTMLElement).dataset.threadId = thread.id;

  const row1 = mkEl("div", "thread-card-row");
  let preview = (thread.messages && thread.messages[0]) ? thread.messages[0].body : "";
  if (preview.length > 50) preview = preview.substring(0, 50) + "\u2026";
  row1.appendChild(mkEl("span", "thread-card-preview", preview));
  const msgCount = thread.messages ? thread.messages.length : 0;
  let metaText = msgCount > 1 ? msgCount + " replies" : "";
  if (metaText) metaText += " \u00B7 ";
  metaText += timeAgo(thread.updated_at);
  row1.appendChild(mkEl("span", "thread-card-meta", metaText));
  card.appendChild(row1);

  if (thread.scope && thread.scope.type === "element" && thread.scope.selector) {
    card.appendChild(mkEl("div", "thread-card-selector", thread.scope.selector));
  }

  card.addEventListener("click", function () { showThreadDetail(thread.id); });
  return card;
}

function renderThreadMessages(thread: Thread): HTMLElement {
  const container = mkEl("div", "thread-messages");
  const msgList = mkEl("div", "thread-messages-list");

  thread.messages.forEach(function (msg: Message) {
    const msgEl = mkEl("div", "message message-" + msg.author);
    const icon = mkEl("span", "message-author-icon");
    icon.innerHTML = msg.author === "agent" ? ICONS.robot : ICONS.chat;
    msgEl.appendChild(icon);
    const body = mkEl("div", "message-body");
    body.appendChild(mkEl("div", "message-text", msg.body));

    const authorLabel = msg.author === "agent" ? "Agent" : "You";
    body.appendChild(mkEl("div", "message-meta", authorLabel + " \u00B7 " + timeAgo(msg.created_at)));
    msgEl.appendChild(body);
    msgList.appendChild(msgEl);
  });

  container.appendChild(msgList);

  markThreadSeen(thread.id);

  const input = mkEl("div", "thread-input");
  const textarea = document.createElement("textarea");
  textarea.className = PREFIX + "textarea";
  textarea.placeholder = "Reply...";
  textarea.rows = 2;
  input.appendChild(textarea);

  const inputActions = mkEl("div", "thread-input-actions");

  const resolveBtn = mkEl("button", "btn btn-secondary btn-sm", "Resolve \u2713");
  resolveBtn.addEventListener("click", function () {
    const text = textarea.value.trim();
    const doResolve = function () {
      api("POST", "/threads/" + thread.id + "/resolve").then(function () {
        thread.status = "resolved";
        dispatch({ type: "SET_THREADS", threads: [...getState().threads] });
        deps().closeActivePopover();
        showThreadList();
        deps().renderAllPins();
        toast("Thread resolved");
      });
    };
    if (text) {
      api("POST", "/threads/" + thread.id + "/messages", { body: text }).then(function (raw) {
        thread.messages.push(raw as Message);
        doResolve();
      });
    } else {
      doResolve();
    }
  });
  inputActions.appendChild(resolveBtn);

  const sendBtn = mkEl("button", "btn btn-primary btn-sm", "Send" + SUBMIT_HINT) as HTMLButtonElement;
  sendBtn.addEventListener("click", function () {
    const text = textarea.value.trim();
    if (!text) return;
    if (sendBtn.disabled) return;
    sendBtn.disabled = true;
    api("POST", "/threads/" + thread.id + "/messages", { body: text }).then(function (raw) {
      thread.messages.push(raw as Message);
      thread.updated_at = new Date().toISOString();
      textarea.value = "";
      sendBtn.disabled = false;
      if (getState().panelOpen) renderPanel();
      deps().renderAllPins();
    }).catch(function () {
      sendBtn.disabled = false;
      toast("Failed to send reply", true);
    });
  });
  inputActions.appendChild(sendBtn);
  submitOnModEnter(textarea, sendBtn);
  input.appendChild(inputActions);
  container.appendChild(input);

  return container;
}

export function markThreadSeen(threadId: string): void {
  const thread = findThread(getState().threads, threadId);
  const lastSeq = thread && thread.messages.length > 0 ? thread.messages.length : 0;
  // Update the persisted seen-count locally so unread clears immediately, then
  // persist it server-side so it survives a reload.
  if (thread) thread.last_human_seen_seq = lastSeq;
  api("PUT", "/threads/" + threadId + "/seen", { seq: lastSeq }).catch(function () {});
  if (thread) deps().addPin(thread);
  updateBadge();
  updateMarkReadBtn();
}

export function markAllRead(): void {
  getState().threads.forEach(function (t: Thread) {
    if (hasUnread(t)) {
      const seenSeq = t.messages.length > 0 ? t.messages.length : 0;
      t.last_human_seen_seq = seenSeq;
      api("PUT", "/threads/" + t.id + "/seen", { seq: seenSeq }).catch(function () {});
    }
  });
  deps().renderAllPins();
  updateBadge();
  updateMarkReadBtn();
  if (getState().panelOpen) renderPanel();
  toast("All marked as read");
}
