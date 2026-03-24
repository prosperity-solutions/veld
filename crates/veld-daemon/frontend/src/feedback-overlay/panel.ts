import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl, findThread, hasUnread, timeAgo, getThreadPageUrl, submitOnModEnter } from "./helpers";
import { PREFIX, ICONS, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { parseControls, renderControls } from "./controls-renderer";
import { toast } from "./toast";
import { updateBadge } from "./badge";
import type { Thread, Message } from "./types";
import { deps } from "../shared/registry";

export function togglePanel(): void {
  dispatch({ type: "SET_PANEL_OPEN", open: !getState().panelOpen });
  if (getState().panelOpen) dispatch({ type: "SET_EXPANDED_THREAD", threadId: null });
  refs.panel.classList.toggle(PREFIX + "panel-open", getState().panelOpen);
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
  const anyUnread = getState().threads.some(function (t: Thread) { return hasUnread(t, getState().lastSeenAt); });
  refs.markReadBtn.style.display = anyUnread ? "" : "none";
}

// Track control cleanups to prevent memory leaks on re-render
let controlCleanups: (() => void)[] = [];

export function renderPanel(): void {
  // Clean up previous control listeners before re-rendering
  controlCleanups.forEach((fn) => fn());
  controlCleanups = [];
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

function renderActiveThreads(): void {
  const active = getState().threads.filter(function (t: Thread) { return t.status === "open"; });
  if (!active.length) {
    refs.panelBody.appendChild(mkEl("div", "panel-empty", "No active threads."));
    return;
  }
  const byPage: Record<string, Thread[]> = {};
  const pageOrder: string[] = [];
  active.forEach(function (t: Thread) {
    const url = getThreadPageUrl(t);
    const path = (url || "/").split("?")[0];
    if (!byPage[path]) { byPage[path] = []; pageOrder.push(path); }
    byPage[path].push(t);
  });
  pageOrder.sort(function (a, b) {
    if (a === window.location.pathname) return -1;
    if (b === window.location.pathname) return 1;
    return a.localeCompare(b);
  });
  pageOrder.forEach(function (p) {
    let label = "Page " + p;
    if (p === "/") label += " (home)";
    renderThreadGroup(label, byPage[p]);
  });
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

function renderThreadGroup(label: string, threads: Thread[]): void {
  threads.sort(function (a: Thread, b: Thread) {
    return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
  });
  const section = mkEl("div", "panel-section");
  section.appendChild(mkEl("div", "panel-section-heading", label));
  threads.forEach(function (t: Thread) { section.appendChild(makeThreadCard(t, false)); });
  refs.panelBody.appendChild(section);
}

function makeThreadCard(thread: Thread, isResolved: boolean): HTMLElement {
  const card = mkEl("div", "thread-card" + (isResolved ? " thread-card-resolved" : ""));
  if (hasUnread(thread, getState().lastSeenAt) && !isResolved) card.classList.add(PREFIX + "thread-card-unread");
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

  if (thread.claimed_by) {
    const claimBadge = mkEl("div", "thread-card-claim-badge");
    const badgeIcon = mkEl("span", "thread-card-claim-icon");
    badgeIcon.innerHTML = ICONS.robot;
    claimBadge.appendChild(badgeIcon);
    claimBadge.appendChild(document.createTextNode(thread.claimed_by));
    if (thread.claimed_at) {
      claimBadge.appendChild(mkEl("span", "thread-card-claim-time", " \u00B7 " + timeAgo(thread.claimed_at)));
    }
    card.appendChild(claimBadge);
    card.classList.add(PREFIX + "thread-card-claimed");
  }

  card.addEventListener("click", function () { showThreadDetail(thread.id); });
  return card;
}

function makeClaimRow(thread: Thread): HTMLElement {
  const claimRow = mkEl("div", "message message-claim");
  const claimIcon = mkEl("span", "message-author-icon");
  claimIcon.innerHTML = ICONS.robot;
  claimRow.appendChild(claimIcon);
  const claimBody = mkEl("div", "message-body");
  const claimText = mkEl("div", "message-claim-text", "Being worked on by " + thread.claimed_by);
  claimBody.appendChild(claimText);
  if (thread.claimed_at) {
    claimBody.appendChild(mkEl("div", "message-meta", timeAgo(thread.claimed_at)));
  }
  claimRow.appendChild(claimBody);
  return claimRow;
}

function renderThreadMessages(thread: Thread): HTMLElement {
  const container = mkEl("div", "thread-messages");
  const msgList = mkEl("div", "thread-messages-list");
  const msgCount = thread.messages.length;

  // If claimed, find where the claim event fits chronologically.
  const claimTime = thread.claimed_by && thread.claimed_at ? new Date(thread.claimed_at).getTime() : null;
  let claimInserted = false;

  thread.messages.forEach(function (msg: Message, msgIndex: number) {
    // Insert claim row before the first message that came after the claim.
    if (claimTime && !claimInserted && new Date(msg.created_at).getTime() > claimTime) {
      msgList.appendChild(makeClaimRow(thread));
      claimInserted = true;
    }

    const msgEl = mkEl("div", "message message-" + msg.author);
    const icon = mkEl("span", "message-author-icon");
    icon.innerHTML = msg.author === "agent" ? ICONS.robot : ICONS.chat;
    msgEl.appendChild(icon);
    const body = mkEl("div", "message-body");
    body.appendChild(mkEl("div", "message-text", msg.body));

    // Render interactive controls if present.
    // Controls are inactive if a later message exists (values were applied).
    const controls = parseControls(msg);
    if (controls && window.__veld_controls) {
      const isLastMessage = msgIndex === msgCount - 1;
      const { element, cleanup } = renderControls(controls, window.__veld_controls, thread.id, {
        inactive: !isLastMessage,
      });
      body.appendChild(element);
      controlCleanups.push(cleanup);
    }

    const authorLabel = msg.author === "agent" ? "Agent" : "You";
    body.appendChild(mkEl("div", "message-meta", authorLabel + " \u00B7 " + timeAgo(msg.created_at)));
    msgEl.appendChild(body);
    msgList.appendChild(msgEl);
  });

  // If claim happened after all messages, append at the end.
  if (claimTime && !claimInserted && thread.claimed_by) {
    msgList.appendChild(makeClaimRow(thread));
  }

  container.appendChild(msgList);

  markThreadSeen(thread.id);

  const input = mkEl("div", "thread-input");
  const textarea = document.createElement("textarea");
  textarea.className = PREFIX + "textarea";
  textarea.placeholder = "Reply...";
  textarea.rows = 2;
  input.appendChild(textarea);

  const inputActions = mkEl("div", "thread-input-actions");

  if (thread.claimed_by) {
    const releaseBtn = mkEl("button", "btn btn-sm btn-release");
    releaseBtn.innerHTML = ICONS.robot + " Release";
    releaseBtn.addEventListener("click", function () {
      api("POST", "/threads/" + thread.id + "/release").then(function () {
        thread.claimed_by = null;
        thread.claimed_at = null;
        dispatch({ type: "SET_THREADS", threads: [...getState().threads] });
        renderPanel();
        toast("Thread released");
      });
    });
    inputActions.appendChild(releaseBtn);
  }

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
  dispatch({ type: "MARK_SEEN", threadId });
  const thread = findThread(getState().threads, threadId);
  const lastSeq = thread && thread.messages.length > 0 ? thread.messages.length : 0;
  api("PUT", "/threads/" + threadId + "/seen", { seq: lastSeq }).catch(function () {});
  if (thread) deps().addPin(thread);
  updateBadge();
  updateMarkReadBtn();
}

export function markAllRead(): void {
  getState().threads.forEach(function (t: Thread) {
    if (hasUnread(t, getState().lastSeenAt)) {
      dispatch({ type: "MARK_SEEN", threadId: t.id });
      const seenSeq = t.messages.length > 0 ? t.messages.length : 0;
      api("PUT", "/threads/" + t.id + "/seen", { seq: seenSeq }).catch(function () {});
    }
  });
  deps().renderAllPins();
  updateBadge();
  updateMarkReadBtn();
  if (getState().panelOpen) renderPanel();
  toast("All marked as read");
}
