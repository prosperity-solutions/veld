import { S } from "./state";
import type { Thread, Message, FeedbackEvent } from "./types";
import { findThread } from "./helpers";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { mkEl } from "./helpers";
import { updateBadge } from "./badge";
import { updateListeningModule } from "./listening";

// Late-bound deps
export let addPinFn: (thread: Thread) => void;
export let removePinFn: (threadId: string) => void;
export let renderAllPinsFn: () => void;
export let renderPanelFn: () => void;
export let openThreadInPanelFn: (threadId: string) => void;
export let scrollToThreadFn: (threadId: string) => void;
export let checkPendingScrollFn: () => void;

export function setPollingDeps(deps: {
  addPin: typeof addPinFn;
  removePin: typeof removePinFn;
  renderAllPins: typeof renderAllPinsFn;
  renderPanel: typeof renderPanelFn;
  openThreadInPanel: typeof openThreadInPanelFn;
  scrollToThread: typeof scrollToThreadFn;
  checkPendingScroll: typeof checkPendingScrollFn;
}) {
  addPinFn = deps.addPin;
  removePinFn = deps.removePin;
  renderAllPinsFn = deps.renderAllPins;
  renderPanelFn = deps.renderPanel;
  openThreadInPanelFn = deps.openThreadInPanel;
  scrollToThreadFn = deps.scrollToThread;
  checkPendingScrollFn = deps.checkPendingScroll;
}

export function pollEvents(): void {
  api("GET", "/events?after=" + S.lastEventSeq).then(function (raw) {
    const events = raw as FeedbackEvent[];
    if (!events || !events.length) return;
    events.forEach(function (event: FeedbackEvent) {
      handleEvent(event);
      if (event.seq > S.lastEventSeq) S.lastEventSeq = event.seq;
    });
  }).catch(function () {});
}

export function pollListenStatus(): void {
  api("GET", "/session").then(function (raw) {
    const data = raw as { listening?: boolean } | null;
    const wasListening = S.agentListening;
    S.agentListening = !!(data && data.listening);
    if (S.agentListening !== wasListening) updateListeningModule();
  }).catch(function () {});
}

function handleEvent(event: FeedbackEvent): void {
  switch (event.event) {
    case "agent_message":
      handleAgentMessage(event);
      break;
    case "agent_thread_created":
      handleAgentThreadCreated(event);
      break;
    case "resolved":
      handleThreadResolved(event);
      break;
    case "reopened":
      handleThreadReopened(event);
      break;
    case "agent_listening":
      S.agentListening = true;
      updateListeningModule();
      break;
    case "agent_stopped":
      S.agentListening = false;
      updateListeningModule();
      toast("Agent stopped listening");
      break;
    case "session_ended":
      S.agentListening = false;
      updateListeningModule();
      break;
    case "thread_created":
      if (event.thread && !findThread(S.threads, event.thread.id)) {
        S.threads.push(event.thread);
        addPinFn(event.thread);
        updateBadge();
        if (S.panelOpen) renderPanelFn();
      }
      break;
    case "human_message":
      if (event.thread_id && event.message) {
        const hmThread = findThread(S.threads, event.thread_id);
        if (hmThread) {
          const exists = hmThread.messages.some(function (m: Message) { return m.id === event.message!.id; });
          if (!exists) {
            hmThread.messages.push(event.message);
            hmThread.updated_at = event.message.created_at || new Date().toISOString();
            if (S.panelOpen) renderPanelFn();
          }
        }
      }
      break;
  }
}

function handleAgentMessage(event: FeedbackEvent): void {
  const thread = findThread(S.threads, event.thread_id!);
  if (!thread) {
    api("GET", "/threads/" + event.thread_id).then(function (raw) {
      const t = raw as Thread;
      if (t) {
        S.threads.push(t);
        addPinFn(t);
        updateBadge();
        if (S.panelOpen) renderPanelFn();
        showAgentReplyToast(t.id, event.message!.body);
      }
    }).catch(function () {});
    return;
  }

  if (event.message) {
    let exists = false;
    for (let i = 0; i < thread.messages.length; i++) {
      if (thread.messages[i].id === event.message.id) { exists = true; break; }
    }
    if (!exists) {
      thread.messages.push(event.message);
      thread.updated_at = event.message.created_at || new Date().toISOString();
    }
  }

  addPinFn(thread);
  updateBadge();
  if (S.panelOpen) renderPanelFn();

  const preview = event.message ? event.message.body : "New reply";
  showAgentReplyToast(event.thread_id!, preview);

  if (!document.hasFocus()) {
    sendBrowserNotification("Agent replied", preview, event.thread_id!);
  }
}

function handleAgentThreadCreated(event: FeedbackEvent): void {
  if (event.thread) {
    const existing = findThread(S.threads, event.thread.id);
    if (!existing) {
      S.threads.push(event.thread);
      addPinFn(event.thread);
      updateBadge();
      if (S.panelOpen) renderPanelFn();

      const preview = event.thread.messages && event.thread.messages[0] ? event.thread.messages[0].body : "New thread";
      showAgentReplyToast(event.thread.id, preview);

      if (!document.hasFocus()) {
        sendBrowserNotification("Agent started a thread", preview, event.thread.id);
      }
    }
  } else {
    loadThreads();
  }
}

function handleThreadResolved(event: FeedbackEvent): void {
  const thread = findThread(S.threads, event.thread_id!);
  if (thread) {
    thread.status = "resolved";
    removePinFn(thread.id);
    updateBadge();
    if (S.panelOpen) renderPanelFn();
  }
}

function handleThreadReopened(event: FeedbackEvent): void {
  const thread = findThread(S.threads, event.thread_id!);
  if (thread) {
    thread.status = "open";
    addPinFn(thread);
    updateBadge();
    if (S.panelOpen) renderPanelFn();
  }
}

export function showAgentReplyToast(threadId: string, preview: string): void {
  const t = mkEl("div", "agent-toast");
  t.appendChild(mkEl("div", "agent-toast-header", "Agent replied"));
  const body = mkEl("div", "agent-toast-body");
  body.textContent = preview.length > 60 ? preview.substring(0, 60) + "..." : preview;
  t.appendChild(body);
  const link = mkEl("button", "agent-toast-link", "Go to thread \u2192");
  link.addEventListener("click", function () {
    t.remove();
    openThreadInPanelFn(threadId);
    scrollToThreadFn(threadId);
  });
  t.appendChild(link);
  S.shadow.appendChild(t);
  requestAnimationFrame(function () { t.classList.add(PREFIX + "agent-toast-show"); });
  setTimeout(function () {
    t.classList.remove(PREFIX + "agent-toast-show");
    setTimeout(function () { t.remove(); }, 300);
  }, 8000);
}

export function sendBrowserNotification(title: string, body: string, threadId: string): void {
  if (!("Notification" in window) || Notification.permission !== "granted") return;
  const n = new Notification(title, {
    body: body,
    icon: "/__veld__/feedback/logo.svg",
    tag: "veld-thread-" + threadId
  });
  n.addEventListener("click", function () {
    window.focus();
    openThreadInPanelFn(threadId);
    scrollToThreadFn(threadId);
    n.close();
  });
}

export function loadThreads(): void {
  api("GET", "/threads").then(function (raw) {
    S.threads = (raw as Thread[]) || [];
    renderAllPinsFn();
    updateBadge();
    checkPendingScrollFn();
    if (S.panelOpen) renderPanelFn();
  }).catch(function () {});
}
