import { S } from "./state";
import { mkEl, findThread, hasUnread, timeAgo, getThreadPageUrl, submitOnModEnter } from "./helpers";
import { PREFIX, ICONS, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { updateBadge } from "./badge";
import type { Thread, Message } from "./types";

// Late-bound deps
export let closeActivePopoverFn: () => void;
export let renderAllPinsFn: () => void;
export let addPinFn: (thread: Thread) => void;
export let scrollToThreadFn: (threadId: string) => void;

export function setPanelDeps(deps: {
  closeActivePopover: typeof closeActivePopoverFn;
  renderAllPins: typeof renderAllPinsFn;
  addPin: typeof addPinFn;
  scrollToThread: typeof scrollToThreadFn;
}) {
  closeActivePopoverFn = deps.closeActivePopover;
  renderAllPinsFn = deps.renderAllPins;
  addPinFn = deps.addPin;
  scrollToThreadFn = deps.scrollToThread;
}

export function togglePanel(): void {
  S.panelOpen = !S.panelOpen;
  if (S.panelOpen) S.expandedThreadId = null;
  S.panel.classList.toggle(PREFIX + "panel-open", S.panelOpen);
  if (S.panelOpen) renderPanel();
}

export function showThreadDetail(threadId: string): void {
  S.expandedThreadId = threadId;
  renderPanel();
}

export function showThreadList(): void {
  S.expandedThreadId = null;
  renderPanel();
}

export function openThreadInPanel(threadId: string): void {
  S.panelOpen = true;
  S.panelTab = "active";
  S.expandedThreadId = threadId;
  S.panel.classList.add(PREFIX + "panel-open");
  renderPanel();
}

function updateSegmentedControl(): void {
  if (S.segBtnActive && S.segBtnResolved) {
    const activeCount = S.threads.filter(function (t: Thread) { return t.status === "open"; }).length;
    const resolvedCount = S.threads.filter(function (t: Thread) { return t.status === "resolved"; }).length;
    S.segBtnActive.textContent = "Active" + (activeCount ? " (" + activeCount + ")" : "");
    S.segBtnResolved.textContent = "Resolved" + (resolvedCount ? " (" + resolvedCount + ")" : "");
    S.segBtnActive.className = PREFIX + "segmented-btn" + (S.panelTab === "active" ? " " + PREFIX + "segmented-btn-active" : "");
    S.segBtnResolved.className = PREFIX + "segmented-btn" + (S.panelTab === "resolved" ? " " + PREFIX + "segmented-btn-active" : "");
  }
}

export function updateMarkReadBtn(): void {
  if (!S.markReadBtn) return;
  const anyUnread = S.threads.some(function (t: Thread) { return hasUnread(t, S.lastSeenAt); });
  S.markReadBtn.style.display = anyUnread ? "" : "none";
}

export function renderPanel(): void {
  S.panelBody.innerHTML = "";

  if (S.expandedThreadId) {
    const thread = findThread(S.threads, S.expandedThreadId);
    if (thread) {
      S.panelBackBtn.style.display = "";
      // Hide segmented control in detail view
      const segControl = S.panelBackBtn.parentElement?.querySelector("." + PREFIX + "segmented") as HTMLElement | null;
      if (segControl) segControl.style.display = "none";
      if (S.markReadBtn) S.markReadBtn.style.display = "none";
      S.panelHeadTitle.textContent = "Thread";
      renderThreadDetail(thread);
      return;
    }
    S.expandedThreadId = null;
  }

  S.panelBackBtn.style.display = "none";
  const segControl = S.panelBackBtn.parentElement?.querySelector("." + PREFIX + "segmented") as HTMLElement | null;
  if (segControl) segControl.style.display = "";
  S.panelHeadTitle.textContent = "Threads";
  updateSegmentedControl();
  updateMarkReadBtn();

  if (S.panelTab === "active") {
    renderActiveThreads();
  } else {
    renderResolvedThreads();
  }
}

const COPY_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>';

function makeCopyRow(label: string, value: string, cls: string): HTMLElement {
  const row = mkEl("div", cls);
  row.appendChild(document.createTextNode(label + value));
  const icon = mkEl("span", "panel-detail-copy-icon");
  icon.innerHTML = COPY_SVG;
  row.appendChild(icon);
  row.addEventListener("click", function (e) {
    e.stopPropagation();
    navigator.clipboard.writeText(value).then(function () {
      icon.innerHTML = ICONS.check;
      setTimeout(function () { icon.innerHTML = COPY_SVG; }, 1500);
    });
  });
  return row;
}

function renderThreadDetail(thread: Thread): void {
  const header = mkEl("div", "panel-detail-header");
  header.appendChild(makeCopyRow("ID: ", thread.id.substring(0, 20) + "\u2026", "panel-detail-id"));

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
      scrollToThreadFn(thread.id);
    });
    header.appendChild(goLink);
  }

  if (thread.component_trace && thread.component_trace.length) {
    header.appendChild(makeCopyRow("", thread.component_trace.join(" > "), "panel-detail-trace"));
  }
  if (thread.scope.type === "element" && thread.scope.selector) {
    header.appendChild(makeCopyRow("", thread.scope.selector, "panel-detail-selector"));
  }

  S.panelBody.appendChild(header);

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
    S.panelBody.appendChild(msgList);

    const reopenRow = mkEl("div", "thread-input-actions");
    const reopenBtn = mkEl("button", "btn btn-primary btn-sm", "Reopen Thread");
    reopenBtn.addEventListener("click", function () {
      api("POST", "/threads/" + thread.id + "/reopen").then(function () {
        thread.status = "open";
        showThreadList();
        renderAllPinsFn();
        toast("Thread reopened");
      });
    });
    reopenRow.appendChild(reopenBtn);
    S.panelBody.appendChild(reopenRow);
  } else {
    S.panelBody.appendChild(renderThreadMessages(thread));
  }
}

function renderActiveThreads(): void {
  const active = S.threads.filter(function (t: Thread) { return t.status === "open"; });
  if (!active.length) {
    S.panelBody.appendChild(mkEl("div", "panel-empty", "No active threads."));
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
  const resolved = S.threads.filter(function (t: Thread) { return t.status === "resolved"; });
  if (!resolved.length) {
    S.panelBody.appendChild(mkEl("div", "panel-empty", "No resolved threads."));
    return;
  }
  resolved.sort(function (a: Thread, b: Thread) {
    return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
  });
  resolved.forEach(function (t: Thread) { S.panelBody.appendChild(makeThreadCard(t, true)); });
}

function renderThreadGroup(label: string, threads: Thread[]): void {
  threads.sort(function (a: Thread, b: Thread) {
    return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
  });
  const section = mkEl("div", "panel-section");
  section.appendChild(mkEl("div", "panel-section-heading", label));
  threads.forEach(function (t: Thread) { section.appendChild(makeThreadCard(t, false)); });
  S.panelBody.appendChild(section);
}

function makeThreadCard(thread: Thread, isResolved: boolean): HTMLElement {
  const card = mkEl("div", "thread-card" + (isResolved ? " thread-card-resolved" : ""));
  if (hasUnread(thread, S.lastSeenAt) && !isResolved) card.classList.add(PREFIX + "thread-card-unread");
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
        closeActivePopoverFn();
        showThreadList();
        renderAllPinsFn();
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
      if (S.panelOpen) renderPanel();
      renderAllPinsFn();
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
  S.lastSeenAt[threadId] = Date.now();
  api("PUT", "/threads/" + threadId + "/seen").catch(function () {});
  const thread = findThread(S.threads, threadId);
  if (thread) addPinFn(thread);
  updateBadge();
  updateMarkReadBtn();
}

export function markAllRead(): void {
  S.threads.forEach(function (t: Thread) {
    if (hasUnread(t, S.lastSeenAt)) {
      S.lastSeenAt[t.id] = Date.now();
      api("PUT", "/threads/" + t.id + "/seen").catch(function () {});
    }
  });
  renderAllPinsFn();
  updateBadge();
  updateMarkReadBtn();
  if (S.panelOpen) renderPanel();
  toast("All marked as read");
}
