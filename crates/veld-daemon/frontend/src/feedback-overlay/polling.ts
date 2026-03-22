import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { Thread, Message, FeedbackEvent } from "./types";
import { findThread } from "./helpers";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { mkEl } from "./helpers";
import { updateBadge } from "./badge";
import { updateListeningModule } from "./listening";
import { deps } from "../shared/registry";

export function pollEvents(): void {
  api("GET", "/events?after=" + getState().lastEventSeq).then(function (raw) {
    const events = raw as FeedbackEvent[];
    if (!events || !events.length) return;
    events.forEach(function (event: FeedbackEvent) {
      handleEvent(event);
      if (event.seq > getState().lastEventSeq) dispatch({ type: "SET_LAST_EVENT_SEQ", seq: event.seq });
    });
  }).catch(function () {});
}

export function pollListenStatus(): void {
  api("GET", "/session").then(function (raw) {
    const data = raw as { listening?: boolean } | null;
    const wasListening = getState().agentListening;
    dispatch({ type: "SET_LISTENING", listening: !!(data && data.listening) });
    if (getState().agentListening !== wasListening) updateListeningModule();
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
      dispatch({ type: "SET_LISTENING", listening: true });
      updateListeningModule();
      break;
    case "agent_stopped":
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
      toast("Agent stopped listening");
      break;
    case "session_ended":
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
      break;
    case "thread_created":
      if (event.thread && !findThread(getState().threads, event.thread.id)) {
        dispatch({ type: "ADD_THREAD", thread: event.thread });
        deps().addPin(event.thread);
        updateBadge();
        if (getState().panelOpen) deps().renderPanel();
      }
      break;
    case "human_message":
      if (event.thread_id && event.message) {
        const hmThread = findThread(getState().threads, event.thread_id);
        if (hmThread) {
          const exists = hmThread.messages.some(function (m: Message) { return m.id === event.message!.id; });
          if (!exists) {
            hmThread.messages.push(event.message);
            hmThread.updated_at = event.message.created_at || new Date().toISOString();
            if (getState().panelOpen) deps().renderPanel();
          }
        }
      }
      break;
  }
}

function handleAgentMessage(event: FeedbackEvent): void {
  const thread = findThread(getState().threads, event.thread_id!);
  if (!thread) {
    api("GET", "/threads/" + event.thread_id).then(function (raw) {
      const t = raw as Thread;
      if (t) {
        dispatch({ type: "ADD_THREAD", thread: t });
        deps().addPin(t);
        updateBadge();
        if (getState().panelOpen) deps().renderPanel();
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

  deps().addPin(thread);
  updateBadge();
  if (getState().panelOpen) deps().renderPanel();

  const preview = event.message ? event.message.body : "New reply";
  showAgentReplyToast(event.thread_id!, preview);

  if (!document.hasFocus()) {
    sendBrowserNotification("Agent replied", preview, event.thread_id!);
  }
}

function handleAgentThreadCreated(event: FeedbackEvent): void {
  if (event.thread) {
    const existing = findThread(getState().threads, event.thread.id);
    if (!existing) {
      dispatch({ type: "ADD_THREAD", thread: event.thread });
      deps().addPin(event.thread);
      updateBadge();
      if (getState().panelOpen) deps().renderPanel();

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
  const thread = findThread(getState().threads, event.thread_id!);
  if (thread) {
    thread.status = "resolved";
    dispatch({ type: "SET_THREADS", threads: [...getState().threads] });
    deps().removePin(thread.id);
    updateBadge();
    if (getState().panelOpen) deps().renderPanel();
  }
}

function handleThreadReopened(event: FeedbackEvent): void {
  const thread = findThread(getState().threads, event.thread_id!);
  if (thread) {
    thread.status = "open";
    dispatch({ type: "SET_THREADS", threads: [...getState().threads] });
    deps().addPin(thread);
    updateBadge();
    if (getState().panelOpen) deps().renderPanel();
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
    deps().openThreadInPanel(threadId);
    deps().scrollToThread(threadId);
  });
  t.appendChild(link);
  refs.shadow.appendChild(t);
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
    deps().openThreadInPanel(threadId);
    deps().scrollToThread(threadId);
    n.close();
  });
}

export function loadThreads(): void {
  api("GET", "/threads").then(function (raw) {
    dispatch({ type: "SET_THREADS", threads: (raw as Thread[]) || [] });
    deps().renderAllPins();
    updateBadge();
    deps().checkPendingScroll();
    if (getState().panelOpen) deps().renderPanel();
  }).catch(function () {});
}
