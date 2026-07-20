import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { Thread, Message, FeedbackEvent } from "./types";
import { findThread } from "./helpers";
import { PREFIX, ICONS } from "./constants";
import { api } from "./api";
import { mkEl } from "./helpers";
import { updateBadge } from "./badge";
import { updateListeningModule } from "./listening";
import { pruneReplyDrafts } from "./persist";
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

/**
 * Baseline the event cursor to the latest seq without replaying history.
 *
 * Called once at (re)load. Without this, the first `pollEvents` (cursor at 0)
 * re-fetches the entire event log and re-fires a toast for every past agent
 * reply — so notifications reappear on every reload. Current thread state comes
 * from `loadThreads()`; the event stream only needs to surface activity that
 * happens *after* load.
 */
export function primeEventSeq(): void {
  api("GET", "/events?after=" + getState().lastEventSeq).then(function (raw) {
    const events = raw as FeedbackEvent[];
    if (!events || !events.length) return;
    const maxSeq = events[events.length - 1].seq; // events are seq-ascending
    if (maxSeq > getState().lastEventSeq) dispatch({ type: "SET_LAST_EVENT_SEQ", seq: maxSeq });
  }).catch(function () {});
}

// Guards the "agent started listening" announcement so it doesn't fire for the
// initial discovery of an already-listening agent on (re)load — only for a
// transition that happens while the page is open.
let listeningPrimed = false;

// Announce the "agent is watching" popover/notification at most once per
// session. The heartbeat can briefly lapse during a long edit (>60s) and flip
// listening false→true again; without this guard that would re-announce every
// work cycle. Reset only on a real session end (agent_stopped / session_ended).
let announcedListening = false;

export function pollListenStatus(): void {
  api("GET", "/session").then(function (raw) {
    const data = raw as { listening?: boolean } | null;
    const wasListening = getState().agentListening;
    const nowListening = !!(data && data.listening);
    dispatch({ type: "SET_LISTENING", listening: nowListening });
    if (nowListening !== wasListening) updateListeningModule();
    if (listeningPrimed && !wasListening && nowListening) notifyAgentListening();
    listeningPrimed = true;
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
      if (!getState().agentListening) notifyAgentListening();
      dispatch({ type: "SET_LISTENING", listening: true });
      updateListeningModule();
      break;
    case "agent_stopped":
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
      announcedListening = false; // a genuinely new session re-announces
      break;
    case "session_ended":
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
      announcedListening = false; // a genuinely new session re-announces
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
  if (getState().panelOpen) return;
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

/** Prominent, auto-dismissing banner shown when an agent starts a session. */
function showListeningPopover(): void {
  const existing = refs.shadow.querySelector("." + PREFIX + "listening-popover");
  if (existing) existing.remove();

  const pop = mkEl("div", "listening-popover");
  let done = false;
  const dismiss = function (): void {
    if (done) return;
    done = true;
    pop.classList.remove(PREFIX + "listening-popover-show");
    setTimeout(function () { pop.remove(); }, 300);
  };

  const title = mkEl("div", "listening-popover-title");
  const icon = mkEl("span", "listening-popover-icon");
  icon.innerHTML = ICONS.robot;
  title.appendChild(icon);
  title.appendChild(document.createTextNode("An agent is watching for your feedback"));
  pop.appendChild(title);
  pop.appendChild(mkEl("div", "listening-popover-body",
    "Click an element, the page, or take a screenshot to leave a comment. Hit Done when you're finished."));
  const actions = mkEl("div", "listening-popover-actions");
  const gotIt = mkEl("button", "btn btn-primary btn-sm", "Got it");
  gotIt.addEventListener("click", dismiss);
  actions.appendChild(gotIt);
  pop.appendChild(actions);

  refs.shadow.appendChild(pop);
  requestAnimationFrame(function () { pop.classList.add(PREFIX + "listening-popover-show"); });
  setTimeout(dismiss, 12000);
}

/** Announce that an agent just started a feedback session: in-page popover
 *  always, browser notification when the tab isn't focused. */
function notifyAgentListening(): void {
  if (announcedListening) return; // already announced for this session
  announcedListening = true;
  showListeningPopover();
  if (!document.hasFocus() && "Notification" in window && Notification.permission === "granted") {
    try {
      const n = new Notification("Veld — agent is listening", {
        body: "An agent is watching for your feedback on this page.",
        icon: "/__veld__/feedback/logo.svg",
        tag: "veld-listening",
      });
      n.addEventListener("click", function () { window.focus(); n.close(); });
    } catch (_) { /* some browsers throw constructing non-persistent notifications */ }
  }
}

export function sendBrowserNotification(title: string, body: string, threadId: string): void {
  if (!("Notification" in window) || Notification.permission !== "granted") return;
  try {
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
  } catch (_) { /* some browsers throw constructing non-persistent notifications */ }
}

export function loadThreads(): void {
  api("GET", "/threads").then(function (raw) {
    const threads = (raw as Thread[]) || [];
    dispatch({ type: "SET_THREADS", threads });
    // Threads are authoritative here (fetch succeeded), so drop reply drafts
    // orphaned by a thread resolved/deleted while the tab was away — they'd
    // render no reply box and never otherwise clear. NOT done on fetch failure
    // (the .catch below), where an empty list would wipe live drafts.
    pruneReplyDrafts(threads.filter((t) => t.status === "open").map((t) => t.id));
    deps().renderAllPins();
    updateBadge();
    deps().checkPendingScroll();
    if (getState().panelOpen) deps().renderPanel();
    // Restore tab-local state saved before a reload (open panel/tab/scroll,
    // expanded thread, open composer). One-shot: no-ops on later loadThreads
    // calls triggered by events.
    deps().restoreSession();
  }).catch(function () {
    // Even if the initial thread fetch fails, restore once at boot so a later
    // event-driven loadThreads can't fire the (one-shot) restore late — after
    // the user has already started interacting with the freshly-booted overlay.
    // Composer/panel restore don't need threads; a saved expanded thread just
    // degrades to the list view when the thread set is empty.
    deps().restoreSession();
  });
}
