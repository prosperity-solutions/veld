//! Feedback overlay assets served by the feedback HTTP server.

/// HTML page that registers the feedback Service Worker.
pub const INSTALLER_HTML: &str = r###"
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Veld Feedback</title>
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      display: flex; align-items: center; justify-content: center;
      min-height: 100vh; background: #0f172a; color: #e2e8f0;
    }
    .msg { text-align: center; }
    .msg h1 { font-size: 1.25rem; font-weight: 500; margin-bottom: 0.5rem; }
    .msg p { font-size: 0.875rem; color: #94a3b8; }
    .error { color: #f87171; display: none; margin-top: 1rem; font-size: 0.875rem; }
  </style>
</head>
<body>
  <div class="msg">
    <h1>Enabling Veld Feedback&hellip;</h1>
    <p>Registering the feedback service worker.</p>
    <p class="error" id="err"></p>
  </div>
  <script>
    (function () {
      if (!("serviceWorker" in navigator)) {
        document.getElementById("err").textContent = "Service Workers are not supported in this browser.";
        document.getElementById("err").style.display = "block";
        return;
      }
      navigator.serviceWorker.register("/__veld__/sw.js", { scope: "/" })
        .then(function () { window.location.href = "/"; })
        .catch(function (e) {
          var el = document.getElementById("err");
          el.textContent = "Failed to register service worker: " + e.message;
          el.style.display = "block";
        });
    })();
  </script>
</body>
</html>
"###;

/// Service Worker that injects the feedback script into HTML responses.
pub const SERVICE_WORKER_JS: &str = r###"
// Veld Feedback Service Worker
// Intercepts navigation requests and injects the feedback overlay script.

self.addEventListener("install", function () {
  self.skipWaiting();
});

self.addEventListener("activate", function (event) {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", function (event) {
  if (event.request.mode !== "navigate") {
    return;
  }

  event.respondWith(
    fetch(event.request).then(function (response) {
      var ct = response.headers.get("content-type") || "";
      if (!ct.includes("text/html")) {
        return response;
      }

      return response.text().then(function (html) {
        var script = '<script src="/__veld__/feedback/script.js"><\/script>';
        if (html.indexOf("/__veld__/feedback/script.js") !== -1) {
          // Script already present, do not inject again.
          return new Response(html, {
            status: response.status,
            statusText: response.statusText,
            headers: response.headers
          });
        }
        var idx = html.lastIndexOf("</body>");
        if (idx !== -1) {
          html = html.slice(0, idx) + script + "\n" + html.slice(idx);
        } else {
          html = html + script;
        }
        return new Response(html, {
          status: response.status,
          statusText: response.statusText,
          headers: response.headers
        });
      });
    })
  );
});
"###;

/// Self-contained feedback overlay UI script.
pub const OVERLAY_JS: &str = r###"
// ---------------------------------------------------------------------------
// Veld Feedback Overlay
// Self-contained feedback UI injected into the host page via the Service Worker.
// All DOM elements and styles are created dynamically. No external dependencies.
// ---------------------------------------------------------------------------

(function () {
  "use strict";

  // Guard: only initialise once.
  if (window.__veld_feedback_initialised) return;
  window.__veld_feedback_initialised = true;

  // ---------- constants ---------------------------------------------------

  var API = "/__veld__/feedback/api";
  var Z = 999999;
  var PREFIX = "veld-feedback-";

  // ---------- state -------------------------------------------------------

  var __veld_comments = [];      // array of comment objects
  var __veld_nextLocalId = 1;    // local counter for new drafts
  var __veld_feedbackMode = false;
  var __veld_hoveredEl = null;
  var __veld_panelOpen = false;
  var __veld_activePopover = null;

  // ---------- helpers -----------------------------------------------------

  /** Generate a reasonably unique CSS selector for an element. */
  function selectorFor(el) {
    if (el.id) return "#" + CSS.escape(el.id);
    var parts = [];
    var cur = el;
    while (cur && cur !== document.body && cur !== document.documentElement) {
      var seg = cur.tagName.toLowerCase();
      if (cur.id) {
        parts.unshift("#" + CSS.escape(cur.id));
        break;
      }
      if (cur.className && typeof cur.className === "string") {
        var classes = cur.className.trim().split(/\s+/).filter(function (c) {
          return c && !c.startsWith(PREFIX);
        });
        if (classes.length) {
          seg += "." + classes.map(CSS.escape).join(".");
        }
      }
      var parent = cur.parentElement;
      if (parent) {
        var siblings = Array.from(parent.children).filter(function (s) {
          return s.tagName === cur.tagName;
        });
        if (siblings.length > 1) {
          seg += ":nth-child(" + (Array.from(parent.children).indexOf(cur) + 1) + ")";
        }
      }
      parts.unshift(seg);
      cur = parent;
    }
    return parts.join(" > ");
  }

  /** Shortcut to create an element with optional classes and text. */
  function mkEl(tag, cls, text) {
    var el = document.createElement(tag);
    if (cls) el.className = PREFIX + cls;
    if (text !== undefined) el.textContent = text;
    return el;
  }

  /** POST / PUT / DELETE JSON helper. */
  function api(method, path, body) {
    var opts = {
      method: method,
      headers: { "Content-Type": "application/json" }
    };
    if (body !== undefined) opts.body = JSON.stringify(body);
    return fetch(API + path, opts).then(function (r) {
      if (!r.ok) throw new Error("API " + method + " " + path + " failed: " + r.status);
      if (r.status === 204) return null;
      return r.json();
    });
  }

  /** Show a brief toast notification. */
  function toast(msg, isError) {
    var t = mkEl("div", "toast", msg);
    if (isError) t.style.background = "#dc2626";
    document.body.appendChild(t);
    requestAnimationFrame(function () { t.classList.add(PREFIX + "toast-show"); });
    setTimeout(function () {
      t.classList.remove(PREFIX + "toast-show");
      setTimeout(function () { t.remove(); }, 300);
    }, 2800);
  }

  /** Rect of an element relative to the document. */
  function docRect(el) {
    var r = el.getBoundingClientRect();
    return {
      x: r.left + window.scrollX,
      y: r.top + window.scrollY,
      width: r.width,
      height: r.height
    };
  }

  // ---------- styles ------------------------------------------------------

  function injectStyles() {
    if (document.getElementById(PREFIX + "styles")) return;
    var style = document.createElement("style");
    style.id = PREFIX + "styles";
    style.textContent = [
      // CSS custom properties
      ":root {",
      "  --vf-primary: #3b82f6;",
      "  --vf-primary-hover: #2563eb;",
      "  --vf-danger: #ef4444;",
      "  --vf-bg-dark: #1e293b;",
      "  --vf-bg-card: #273449;",
      "  --vf-text: #f1f5f9;",
      "  --vf-text-muted: #94a3b8;",
      "  --vf-border: #334155;",
      "  --vf-shadow: 0 8px 30px rgba(0,0,0,.35);",
      "  --vf-radius: 10px;",
      "  --vf-z: " + Z + ";",
      "}",

      // Toast
      "." + PREFIX + "toast {",
      "  position: fixed; bottom: 100px; left: 50%; transform: translateX(-50%) translateY(20px);",
      "  background: var(--vf-primary); color: #fff; padding: 10px 22px; border-radius: 8px;",
      "  font: 500 14px/1.4 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;",
      "  z-index: calc(var(--vf-z) + 10); opacity: 0; transition: opacity .3s, transform .3s;",
      "  pointer-events: none; white-space: nowrap;",
      "}",
      "." + PREFIX + "toast-show { opacity: 1; transform: translateX(-50%) translateY(0); }",

      // FAB container
      "." + PREFIX + "fab-container {",
      "  position: fixed; bottom: 24px; right: 24px; display: flex; align-items: center;",
      "  gap: 10px; z-index: var(--vf-z);",
      "}",

      // FAB
      "." + PREFIX + "fab {",
      "  width: 54px; height: 54px; border-radius: 50%; border: none; cursor: pointer;",
      "  background: var(--vf-primary); color: #fff; font-size: 24px; display: flex;",
      "  align-items: center; justify-content: center; box-shadow: var(--vf-shadow);",
      "  transition: background .2s, transform .15s; position: relative;",
      "}",
      "." + PREFIX + "fab:hover { background: var(--vf-primary-hover); transform: scale(1.07); }",
      "." + PREFIX + "fab-active { background: var(--vf-danger) !important; }",
      "." + PREFIX + "fab-active:hover { background: #dc2626 !important; }",

      // Badge
      "." + PREFIX + "badge {",
      "  position: absolute; top: -4px; right: -4px; min-width: 20px; height: 20px;",
      "  border-radius: 10px; background: var(--vf-danger); color: #fff; font-size: 11px;",
      "  font-weight: 700; display: flex; align-items: center; justify-content: center;",
      "  padding: 0 5px; pointer-events: none;",
      "}",
      "." + PREFIX + "badge-hidden { display: none; }",

      // Panel toggle button (list icon)
      "." + PREFIX + "panel-btn {",
      "  width: 40px; height: 40px; border-radius: 50%; border: none; cursor: pointer;",
      "  background: var(--vf-bg-dark); color: var(--vf-text); font-size: 18px;",
      "  display: flex; align-items: center; justify-content: center;",
      "  box-shadow: var(--vf-shadow); transition: background .2s;",
      "}",
      "." + PREFIX + "panel-btn:hover { background: var(--vf-bg-card); }",

      // Feedback mode overlay
      "." + PREFIX + "overlay {",
      "  position: fixed; inset: 0; z-index: calc(var(--vf-z) - 2);",
      "  background: rgba(15,23,42,.12); pointer-events: none; display: none;",
      "}",
      "." + PREFIX + "overlay-active { display: block; }",

      // Hover outline
      "." + PREFIX + "hover-outline {",
      "  position: absolute; border: 2px dashed var(--vf-primary); pointer-events: none;",
      "  z-index: calc(var(--vf-z) - 1); border-radius: 3px;",
      "  transition: top .1s, left .1s, width .1s, height .1s;",
      "  display: none;",
      "}",

      // Popover (comment create / detail)
      "." + PREFIX + "popover {",
      "  position: absolute; z-index: var(--vf-z); width: 320px;",
      "  background: var(--vf-bg-dark); color: var(--vf-text); border-radius: var(--vf-radius);",
      "  box-shadow: var(--vf-shadow); font: 14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;",
      "  overflow: hidden; animation: " + PREFIX + "fadeIn .15s ease;",
      "}",
      "@keyframes " + PREFIX + "fadeIn { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: translateY(0); } }",

      "." + PREFIX + "popover-header {",
      "  padding: 10px 14px; background: var(--vf-bg-card); font-size: 12px;",
      "  color: var(--vf-text-muted); white-space: nowrap; overflow: hidden;",
      "  text-overflow: ellipsis; border-bottom: 1px solid var(--vf-border);",
      "}",
      "." + PREFIX + "popover-body { padding: 14px; }",

      "." + PREFIX + "textarea {",
      "  width: 100%; min-height: 80px; resize: vertical; background: var(--vf-bg-card);",
      "  color: var(--vf-text); border: 1px solid var(--vf-border); border-radius: 6px;",
      "  padding: 8px 10px; font: inherit; outline: none;",
      "}",
      "." + PREFIX + "textarea:focus { border-color: var(--vf-primary); }",

      "." + PREFIX + "popover-actions {",
      "  display: flex; gap: 8px; justify-content: flex-end; margin-top: 10px;",
      "}",

      "." + PREFIX + "btn {",
      "  padding: 6px 14px; border-radius: 6px; border: none; cursor: pointer;",
      "  font: 500 13px/1.4 inherit; transition: background .15s;",
      "}",
      "." + PREFIX + "btn-primary { background: var(--vf-primary); color: #fff; }",
      "." + PREFIX + "btn-primary:hover { background: var(--vf-primary-hover); }",
      "." + PREFIX + "btn-secondary { background: var(--vf-border); color: var(--vf-text); }",
      "." + PREFIX + "btn-secondary:hover { background: #475569; }",
      "." + PREFIX + "btn-danger { background: var(--vf-danger); color: #fff; }",
      "." + PREFIX + "btn-danger:hover { background: #dc2626; }",
      "." + PREFIX + "btn-sm { padding: 4px 10px; font-size: 12px; }",

      // Pins
      "." + PREFIX + "pin {",
      "  position: absolute; z-index: calc(var(--vf-z) - 1); width: 26px; height: 26px;",
      "  border-radius: 50%; background: var(--vf-primary); color: #fff; font-size: 12px;",
      "  font-weight: 700; display: flex; align-items: center; justify-content: center;",
      "  cursor: pointer; box-shadow: 0 2px 8px rgba(0,0,0,.4);",
      "  transition: transform .15s; border: 2px solid #fff;",
      "}",
      "." + PREFIX + "pin:hover { transform: scale(1.2); }",

      // Side panel
      "." + PREFIX + "panel {",
      "  position: fixed; top: 0; right: 0; bottom: 0; width: 360px; max-width: 90vw;",
      "  background: var(--vf-bg-dark); color: var(--vf-text); z-index: var(--vf-z);",
      "  box-shadow: -4px 0 30px rgba(0,0,0,.4); display: flex; flex-direction: column;",
      "  font: 14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;",
      "  transform: translateX(100%); transition: transform .25s ease;",
      "}",
      "." + PREFIX + "panel-open { transform: translateX(0); }",

      "." + PREFIX + "panel-head {",
      "  padding: 16px 18px; font-size: 16px; font-weight: 600;",
      "  border-bottom: 1px solid var(--vf-border); display: flex;",
      "  align-items: center; justify-content: space-between;",
      "}",
      "." + PREFIX + "panel-close {",
      "  background: none; border: none; color: var(--vf-text-muted); cursor: pointer;",
      "  font-size: 20px; line-height: 1; padding: 0 2px;",
      "}",
      "." + PREFIX + "panel-close:hover { color: var(--vf-text); }",

      "." + PREFIX + "panel-body {",
      "  flex: 1; overflow-y: auto; padding: 12px 18px;",
      "}",

      "." + PREFIX + "panel-item {",
      "  background: var(--vf-bg-card); border-radius: 8px; padding: 12px;",
      "  margin-bottom: 10px; border: 1px solid var(--vf-border);",
      "}",
      "." + PREFIX + "panel-item-selector {",
      "  font-size: 11px; color: var(--vf-text-muted); margin-bottom: 6px;",
      "  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;",
      "}",
      "." + PREFIX + "panel-item-comment {",
      "  font-size: 13px; margin-bottom: 8px; white-space: pre-wrap; word-break: break-word;",
      "}",
      "." + PREFIX + "panel-item-actions { display: flex; gap: 6px; }",

      "." + PREFIX + "panel-footer {",
      "  padding: 14px 18px; border-top: 1px solid var(--vf-border);",
      "}",
      "." + PREFIX + "panel-footer .veld-feedback-btn { width: 100%; text-align: center; padding: 10px; }",

      "." + PREFIX + "panel-empty {",
      "  text-align: center; color: var(--vf-text-muted); padding: 40px 0; font-size: 13px;",
      "}",

      // Selection tooltip
      "." + PREFIX + "sel-tip {",
      "  position: absolute; z-index: var(--vf-z); background: var(--vf-primary);",
      "  color: #fff; padding: 5px 12px; border-radius: 6px; font: 500 12px/1.4 sans-serif;",
      "  cursor: pointer; box-shadow: 0 2px 10px rgba(0,0,0,.3);",
      "  animation: " + PREFIX + "fadeIn .12s ease; white-space: nowrap;",
      "}",
      "." + PREFIX + "sel-tip:hover { background: var(--vf-primary-hover); }",

      // Comment text preview in detail popover
      "." + PREFIX + "comment-text {",
      "  white-space: pre-wrap; word-break: break-word; font-size: 13px; margin-bottom: 8px;",
      "}"
    ].join("\n");
    document.head.appendChild(style);
  }

  // ---------- DOM scaffolding ---------------------------------------------

  var fabContainer, fab, fabBadge, panelBtn;
  var overlay, hoverOutline;
  var panel, panelBody, panelFooter;
  var selTip;

  function buildDOM() {
    // Overlay
    overlay = mkEl("div", "overlay");
    document.body.appendChild(overlay);

    // Hover outline
    hoverOutline = mkEl("div", "hover-outline");
    document.body.appendChild(hoverOutline);

    // FAB container
    fabContainer = mkEl("div", "fab-container");

    panelBtn = mkEl("button", "panel-btn");
    panelBtn.title = "Comment list";
    panelBtn.innerHTML = "&#9776;"; // hamburger
    panelBtn.addEventListener("click", togglePanel);
    fabContainer.appendChild(panelBtn);

    fab = mkEl("button", "fab");
    fab.title = "Toggle feedback mode";
    fab.innerHTML = "&#128172;"; // speech balloon
    fabBadge = mkEl("span", "badge badge-hidden");
    fab.appendChild(fabBadge);
    fab.addEventListener("click", toggleFeedbackMode);
    fabContainer.appendChild(fab);

    document.body.appendChild(fabContainer);

    // Panel
    panel = mkEl("div", "panel");

    var panelHead = mkEl("div", "panel-head");
    panelHead.appendChild(mkEl("span", null, "Feedback Comments"));
    var closeBtn = mkEl("button", "panel-close");
    closeBtn.innerHTML = "&times;";
    closeBtn.addEventListener("click", togglePanel);
    panelHead.appendChild(closeBtn);
    panel.appendChild(panelHead);

    panelBody = mkEl("div", "panel-body");
    panel.appendChild(panelBody);

    panelFooter = mkEl("div", "panel-footer");
    var submitBtn = mkEl("button", "btn btn-primary", "Submit All Feedback");
    submitBtn.addEventListener("click", submitAll);
    panelFooter.appendChild(submitBtn);
    panel.appendChild(panelFooter);

    document.body.appendChild(panel);
  }

  // ---------- badge -------------------------------------------------------

  function updateBadge() {
    var count = __veld_comments.length;
    fabBadge.textContent = count;
    if (count > 0) {
      fabBadge.className = PREFIX + "badge";
    } else {
      fabBadge.className = PREFIX + "badge " + PREFIX + "badge-hidden";
    }
  }

  // ---------- feedback mode -----------------------------------------------

  function toggleFeedbackMode() {
    __veld_feedbackMode = !__veld_feedbackMode;
    if (__veld_feedbackMode) {
      fab.classList.add(PREFIX + "fab-active");
      overlay.classList.add(PREFIX + "overlay-active");
    } else {
      fab.classList.remove(PREFIX + "fab-active");
      overlay.classList.remove(PREFIX + "overlay-active");
      hoverOutline.style.display = "none";
      __veld_hoveredEl = null;
      removeSelTip();
      closeActivePopover();
    }
  }

  // ---------- hover highlight ---------------------------------------------

  function onMouseMove(e) {
    if (!__veld_feedbackMode) return;
    var target = document.elementFromPoint(e.clientX, e.clientY);
    if (!target || isOwnElement(target)) {
      hoverOutline.style.display = "none";
      __veld_hoveredEl = null;
      return;
    }
    __veld_hoveredEl = target;
    var r = target.getBoundingClientRect();
    hoverOutline.style.display = "block";
    hoverOutline.style.top = (r.top + window.scrollY) + "px";
    hoverOutline.style.left = (r.left + window.scrollX) + "px";
    hoverOutline.style.width = r.width + "px";
    hoverOutline.style.height = r.height + "px";
  }

  function isOwnElement(el) {
    while (el) {
      if (el.className && typeof el.className === "string" && el.className.indexOf(PREFIX) !== -1) return true;
      el = el.parentElement;
    }
    return false;
  }

  // ---------- click to create comment -------------------------------------

  function onDocClick(e) {
    if (!__veld_feedbackMode) return;
    if (isOwnElement(e.target)) return;

    e.preventDefault();
    e.stopPropagation();

    var target = __veld_hoveredEl || e.target;
    var sel = window.getSelection();
    var selectedText = sel ? sel.toString().trim() : "";
    var rect = docRect(target);
    var selector = selectorFor(target);
    var tagInfo = target.tagName.toLowerCase();
    if (target.className && typeof target.className === "string") {
      var cls = target.className.trim().split(/\s+/).filter(function (c) { return !c.startsWith(PREFIX); });
      if (cls.length) tagInfo += "." + cls.slice(0, 3).join(".");
    }

    showCreatePopover(rect, selector, tagInfo, selectedText, target);
  }

  // ---------- text selection tooltip --------------------------------------

  function onMouseUp() {
    if (!__veld_feedbackMode) return;
    removeSelTip();
    var sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.toString().trim()) return;

    var range = sel.getRangeAt(0);
    var rects = range.getClientRects();
    if (!rects.length) return;
    var last = rects[rects.length - 1];

    selTip = mkEl("div", "sel-tip", "Add Comment");
    selTip.style.top = (last.bottom + window.scrollY + 6) + "px";
    selTip.style.left = (last.right + window.scrollX) + "px";
    selTip.addEventListener("click", function (e) {
      e.stopPropagation();
      var target = range.startContainer.parentElement;
      if (!target) return;
      var selectedText = sel.toString().trim();
      var rect = docRect(target);
      var selector = selectorFor(target);
      var tagInfo = target.tagName.toLowerCase();
      removeSelTip();
      showCreatePopover(rect, selector, tagInfo, selectedText, target);
    });
    document.body.appendChild(selTip);
  }

  function removeSelTip() {
    if (selTip) { selTip.remove(); selTip = null; }
  }

  // ---------- create popover ----------------------------------------------

  function showCreatePopover(rect, selector, tagInfo, selectedText, targetEl) {
    closeActivePopover();

    var pop = mkEl("div", "popover");
    positionPopover(pop, rect);

    var header = mkEl("div", "popover-header", tagInfo);
    pop.appendChild(header);

    var body = mkEl("div", "popover-body");
    var ta = mkEl("textarea", "textarea");
    ta.placeholder = "Add your feedback\u2026";
    body.appendChild(ta);

    var actions = mkEl("div", "popover-actions");
    var cancelBtn = mkEl("button", "btn btn-secondary", "Cancel");
    cancelBtn.addEventListener("click", function () { closeActivePopover(); });
    var saveBtn = mkEl("button", "btn btn-primary", "Save");
    saveBtn.addEventListener("click", function () {
      var text = ta.value.trim();
      if (!text) { ta.focus(); return; }
      saveComment(selector, selectedText, text, rect, targetEl, pop);
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(saveBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    document.body.appendChild(pop);
    __veld_activePopover = pop;
    ta.focus();
  }

  function positionPopover(pop, rect) {
    var topPos = rect.y + rect.height + 10;
    var leftPos = Math.max(10, Math.min(rect.x, window.innerWidth + window.scrollX - 340));
    pop.style.top = topPos + "px";
    pop.style.left = leftPos + "px";
  }

  function closeActivePopover() {
    if (__veld_activePopover) {
      __veld_activePopover.remove();
      __veld_activePopover = null;
    }
  }

  // ---------- save comment ------------------------------------------------

  function saveComment(selector, selectedText, comment, position, targetEl, popoverEl) {
    var payload = {
      page_url: window.location.pathname + window.location.search,
      element_selector: selector,
      selected_text: selectedText || "",
      comment: comment,
      position: { x: position.x, y: position.y, width: position.width, height: position.height }
    };

    api("POST", "/comments", payload)
      .then(function (res) {
        var c = res || payload;
        if (!c.id) c.id = "local_" + (__veld_nextLocalId++);
        __veld_comments.push(c);
        updateBadge();
        renderPanel();
        addPin(c);
        closeActivePopover();
        toast("Comment saved");
      })
      .catch(function (err) {
        toast("Failed to save comment: " + err.message, true);
      });
  }

  // ---------- pins --------------------------------------------------------

  var __veld_pins = {}; // id -> pin element

  function addPin(comment) {
    removePin(comment.id);
    var idx = __veld_comments.indexOf(comment);
    var num = idx >= 0 ? idx + 1 : Object.keys(__veld_pins).length + 1;

    var pin = mkEl("div", "pin", String(num));
    pin.dataset.commentId = comment.id;
    pin.style.top = (comment.position.y - 13) + "px";
    pin.style.left = (comment.position.x + comment.position.width - 13) + "px";
    pin.addEventListener("click", function (e) {
      e.stopPropagation();
      showDetailPopover(comment, pin);
    });
    document.body.appendChild(pin);
    __veld_pins[comment.id] = pin;
  }

  function removePin(id) {
    if (__veld_pins[id]) { __veld_pins[id].remove(); delete __veld_pins[id]; }
  }

  function renderAllPins() {
    // Remove old pins
    Object.keys(__veld_pins).forEach(function (id) { __veld_pins[id].remove(); });
    __veld_pins = {};
    __veld_comments.forEach(addPin);
  }

  /** Reposition pins when the user scrolls or resizes (elements may have moved). */
  function repositionPins() {
    __veld_comments.forEach(function (c) {
      var pin = __veld_pins[c.id];
      if (!pin) return;
      // Try to find the element to get its current position.
      try {
        var el = document.querySelector(c.element_selector);
        if (el) {
          var r = docRect(el);
          c.position = { x: r.x, y: r.y, width: r.width, height: r.height };
          pin.style.top = (r.y - 13) + "px";
          pin.style.left = (r.x + r.width - 13) + "px";
        }
      } catch (_) { /* selector may be invalid */ }
    });
  }

  var __veld_rafPending = false;
  function scheduleReposition() {
    if (__veld_rafPending) return;
    __veld_rafPending = true;
    requestAnimationFrame(function () {
      __veld_rafPending = false;
      repositionPins();
    });
  }

  // ---------- detail popover ----------------------------------------------

  function showDetailPopover(comment, pinEl) {
    closeActivePopover();

    var pop = mkEl("div", "popover");
    var pinRect = pinEl.getBoundingClientRect();
    pop.style.top = (pinRect.bottom + window.scrollY + 8) + "px";
    pop.style.left = Math.max(10, pinRect.left + window.scrollX - 140) + "px";

    var header = mkEl("div", "popover-header", comment.element_selector);
    pop.appendChild(header);

    var body = mkEl("div", "popover-body");

    // Comment text (or edit textarea)
    var textEl = mkEl("div", "comment-text", comment.comment);
    body.appendChild(textEl);

    var actions = mkEl("div", "popover-actions");
    var editBtn = mkEl("button", "btn btn-secondary btn-sm", "Edit");
    var delBtn = mkEl("button", "btn btn-danger btn-sm", "Delete");
    var cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Close");
    cancelBtn.addEventListener("click", function () { closeActivePopover(); });

    editBtn.addEventListener("click", function () {
      // Replace text with textarea
      var ta = mkEl("textarea", "textarea");
      ta.value = comment.comment;
      textEl.replaceWith(ta);
      ta.focus();

      // Swap buttons
      editBtn.style.display = "none";
      var saveBtn = mkEl("button", "btn btn-primary btn-sm", "Save");
      saveBtn.addEventListener("click", function () {
        var newText = ta.value.trim();
        if (!newText) { ta.focus(); return; }
        api("PUT", "/comments/" + encodeURIComponent(comment.id), { comment: newText })
          .then(function () {
            comment.comment = newText;
            renderPanel();
            closeActivePopover();
            toast("Comment updated");
          })
          .catch(function (err) { toast("Update failed: " + err.message, true); });
      });
      actions.insertBefore(saveBtn, delBtn);
    });

    delBtn.addEventListener("click", function () {
      if (!confirm("Delete this comment?")) return;
      api("DELETE", "/comments/" + encodeURIComponent(comment.id))
        .then(function () {
          __veld_comments = __veld_comments.filter(function (c) { return c.id !== comment.id; });
          removePin(comment.id);
          updateBadge();
          renderPanel();
          closeActivePopover();
          toast("Comment deleted");
        })
        .catch(function (err) { toast("Delete failed: " + err.message, true); });
    });

    actions.appendChild(editBtn);
    actions.appendChild(delBtn);
    actions.appendChild(cancelBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    document.body.appendChild(pop);
    __veld_activePopover = pop;
  }

  // ---------- panel -------------------------------------------------------

  function togglePanel() {
    __veld_panelOpen = !__veld_panelOpen;
    if (__veld_panelOpen) {
      panel.classList.add(PREFIX + "panel-open");
      renderPanel();
    } else {
      panel.classList.remove(PREFIX + "panel-open");
    }
  }

  function renderPanel() {
    panelBody.innerHTML = "";

    if (__veld_comments.length === 0) {
      panelBody.appendChild(mkEl("div", "panel-empty", "No comments yet. Click elements on the page to add feedback."));
      panelFooter.style.display = "none";
      return;
    }
    panelFooter.style.display = "";

    __veld_comments.forEach(function (c, i) {
      var item = mkEl("div", "panel-item");

      var sel = mkEl("div", "panel-item-selector", (i + 1) + ". " + c.element_selector);
      item.appendChild(sel);

      var txt = mkEl("div", "panel-item-comment", c.comment);
      item.appendChild(txt);

      var acts = mkEl("div", "panel-item-actions");

      var editBtn = mkEl("button", "btn btn-secondary btn-sm", "Edit");
      editBtn.addEventListener("click", function () { startInlineEdit(c, item, i); });
      acts.appendChild(editBtn);

      var delBtn = mkEl("button", "btn btn-danger btn-sm", "Delete");
      delBtn.addEventListener("click", function () {
        if (!confirm("Delete this comment?")) return;
        api("DELETE", "/comments/" + encodeURIComponent(c.id))
          .then(function () {
            __veld_comments = __veld_comments.filter(function (x) { return x.id !== c.id; });
            removePin(c.id);
            updateBadge();
            renderPanel();
            toast("Comment deleted");
          })
          .catch(function (err) { toast("Delete failed: " + err.message, true); });
      });
      acts.appendChild(delBtn);

      item.appendChild(acts);
      panelBody.appendChild(item);
    });
  }

  function startInlineEdit(comment, itemEl, idx) {
    itemEl.innerHTML = "";

    var sel = mkEl("div", "panel-item-selector", (idx + 1) + ". " + comment.element_selector);
    itemEl.appendChild(sel);

    var ta = mkEl("textarea", "textarea");
    ta.value = comment.comment;
    itemEl.appendChild(ta);

    var acts = mkEl("div", "popover-actions");
    acts.style.marginTop = "8px";

    var saveBtn = mkEl("button", "btn btn-primary btn-sm", "Save");
    saveBtn.addEventListener("click", function () {
      var newText = ta.value.trim();
      if (!newText) { ta.focus(); return; }
      api("PUT", "/comments/" + encodeURIComponent(comment.id), { comment: newText })
        .then(function () {
          comment.comment = newText;
          renderPanel();
          toast("Comment updated");
        })
        .catch(function (err) { toast("Update failed: " + err.message, true); });
    });
    acts.appendChild(saveBtn);

    var cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Cancel");
    cancelBtn.addEventListener("click", function () { renderPanel(); });
    acts.appendChild(cancelBtn);

    itemEl.appendChild(acts);
    ta.focus();
  }

  // ---------- submit all --------------------------------------------------

  function submitAll() {
    api("POST", "/submit", { page_url: window.location.pathname + window.location.search })
      .then(function () {
        __veld_comments = [];
        renderAllPins();
        updateBadge();
        renderPanel();
        toast("Feedback submitted successfully!");
        if (__veld_panelOpen) togglePanel();
      })
      .catch(function (err) { toast("Submit failed: " + err.message, true); });
  }

  // ---------- keyboard shortcut -------------------------------------------

  function onKeyDown(e) {
    if (e.key === "Escape" && __veld_feedbackMode) {
      if (__veld_activePopover) {
        closeActivePopover();
      } else {
        toggleFeedbackMode();
      }
    }
  }

  // ---------- fetch existing comments on load -----------------------------

  function loadExisting() {
    var pageUrl = window.location.pathname + window.location.search;
    api("GET", "/comments?page_url=" + encodeURIComponent(pageUrl))
      .then(function (data) {
        if (Array.isArray(data) && data.length) {
          __veld_comments = data;
          updateBadge();
          renderAllPins();
        }
      })
      .catch(function () {
        // API may not be ready yet; silently ignore.
      });
  }

  // ---------- init --------------------------------------------------------

  function init() {
    injectStyles();
    buildDOM();
    updateBadge();

    document.addEventListener("mousemove", onMouseMove, true);
    document.addEventListener("click", onDocClick, true);
    document.addEventListener("mouseup", onMouseUp, true);
    document.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", scheduleReposition, true);
    window.addEventListener("resize", scheduleReposition);

    loadExisting();
  }

  // Start when DOM is ready.
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
"###;
