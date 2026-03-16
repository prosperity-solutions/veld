// ---------------------------------------------------------------------------
// Veld Feedback Overlay
// Injected into the host page via Caddy's replace-response handler.
// CSS is loaded externally. No other dependencies.
// ---------------------------------------------------------------------------

(function () {
  "use strict";

  // Guard: only initialise once.
  if (window.__veld_feedback_initialised) return;
  window.__veld_feedback_initialised = true;

  // ---------- constants ---------------------------------------------------

  var API = "/__veld__/feedback/api";
  var PREFIX = "veld-feedback-";
  var IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform);
  // Per-key labels: Mac uses symbols, others use text.
  var KEY_MOD   = IS_MAC ? "\u2318" : "Ctrl";   // ⌘
  var KEY_SHIFT = IS_MAC ? "\u21E7" : "Shift";   // ⇧

  // Trusted SVG icon literals only — never insert user content via innerHTML.
  var ICONS = {
    logo: '<svg viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M13.2 28L4 4H8.4L15.7 23.8H15.8L23.1 4H27.5L18.3 28H13.2Z" fill="currentColor"/><path d="M24.5 29C25.88 29 27 27.88 27 26.5C27 25.12 25.88 24 24.5 24C23.12 24 22 25.12 22 26.5C22 27.88 23.12 29 24.5 29Z" fill="#C4F56A"/></svg>',
    crosshair: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="2" x2="12" y2="6"/><line x1="12" y1="18" x2="12" y2="22"/><line x1="2" y1="12" x2="6" y2="12"/><line x1="18" y1="12" x2="22" y2="12"/></svg>',
    chat: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"/></svg>',
    pageComment: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="8" y1="13" x2="16" y2="13"/><line x1="8" y1="17" x2="12" y2="17"/></svg>',
    send: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>',
    screenshot: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M8 3v4M16 3v4M3 8h4M3 16h4M17 8h4M17 16h4M8 17v4M16 17v4"/></svg>',
    check: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>',
    eyeOff: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M17.94 17.94A10.07 10.07 0 0112 20c-7 0-11-8-11-8a18.45 18.45 0 015.06-5.94M9.9 4.24A9.12 9.12 0 0112 4c7 0 11 8 11 8a18.5 18.5 0 01-2.16 3.19m-6.72-1.07a3 3 0 11-4.24-4.24"/><line x1="1" y1="1" x2="23" y2="23"/></svg>',
    cancel: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>'
  };

  // ---------- state -------------------------------------------------------

  var __veld_comments = []; // all comments across all pages
  var __veld_nextLocalId = 1;
  var __veld_activeMode = null; // null | 'select-element' | 'screenshot'
  var __veld_hoveredEl = null;
  var __veld_lockedEl = null; // element locked while comment popover is open
  var __veld_panelOpen = false;
  var __veld_activePopover = null;
  var __veld_toolbarOpen = false;
  var __veld_hidden = false;
  var __veld_waitActive = false; // CLI is waiting for feedback
  var __veld_waitId = null; // current wait-session ID
  var __veld_waitModalEl = null; // the "someone wants your feedback" modal
  var __veld_notificationSent = false; // browser notification already sent this session

  // ---------- helpers -----------------------------------------------------

  /** Is the shortcut modifier key pressed? Cmd on Mac, Ctrl elsewhere. */
  function modKey(e) {
    return IS_MAC ? e.metaKey : e.ctrlKey;
  }

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

  function mkEl(tag, cls, text) {
    var el = document.createElement(tag);
    if (cls) el.className = cls.split(" ").map(function (c) { return PREFIX + c; }).join(" ");
    if (text !== undefined) el.textContent = text;
    return el;
  }

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

  function docRect(el) {
    var r = el.getBoundingClientRect();
    return {
      x: r.left + window.scrollX,
      y: r.top + window.scrollY,
      width: r.width,
      height: r.height
    };
  }

  // ---------- component trace detection -----------------------------------

  function getComponentTrace(el) {
    var trace = [];
    var MAX_DEPTH = 100;

    // React: __reactFiber$* key
    var fiber = getReactFiber(el);
    if (fiber) {
      var node = fiber;
      var depth = 0;
      while (node && depth++ < MAX_DEPTH) {
        var name = getFiberName(node);
        if (name) trace.unshift(name);
        node = node.return;
      }
      if (trace.length) return trace;
    }

    // Vue 3: __vueParentComponent
    if (el.__vueParentComponent) {
      var inst = el.__vueParentComponent;
      var depth2 = 0;
      while (inst && depth2++ < MAX_DEPTH) {
        var vName = inst.type && (inst.type.name || inst.type.__name);
        if (vName) trace.unshift(vName);
        inst = inst.parent;
      }
      if (trace.length) return trace;
    }

    // Vue 2: __vue__
    if (el.__vue__) {
      var vm = el.__vue__;
      var depth3 = 0;
      while (vm && depth3++ < MAX_DEPTH) {
        var vmName = vm.$options && vm.$options.name;
        if (vmName) trace.unshift(vmName);
        vm = vm.$parent;
      }
      if (trace.length) return trace;
    }

    return null;
  }

  function getReactFiber(el) {
    var keys = Object.keys(el);
    for (var i = 0; i < keys.length; i++) {
      if (keys[i].startsWith("__reactFiber$") || keys[i].startsWith("__reactInternalInstance$")) {
        return el[keys[i]];
      }
    }
    return null;
  }

  function getFiberName(fiber) {
    if (!fiber || !fiber.type) return null;
    if (typeof fiber.type === "string") return null;
    return fiber.type.displayName || fiber.type.name || null;
  }

  function formatTrace(trace) {
    if (!trace || !trace.length) return null;
    var deduped = [trace[0]];
    for (var i = 1; i < trace.length; i++) {
      if (trace[i] !== trace[i - 1]) deduped.push(trace[i]);
    }
    if (deduped.length > 5) deduped = deduped.slice(deduped.length - 5);
    return deduped.join(" > ");
  }

  // ---------- DOM scaffolding ---------------------------------------------

  var toolbarContainer, fab, fabBadge, toolbar;
  var toolBtnSelect, toolBtnScreenshot, toolBtnPageComment, toolBtnComments, toolBtnSubmit, toolBtnApprove, toolBtnCancel, toolBtnHide;
  var overlay, hoverOutline, componentTraceEl;
  var screenshotRect; // the selection rectangle element
  var panel, panelBody, panelFooter;

  function buildDOM() {
    initTooltip();

    overlay = mkEl("div", "overlay");
    document.body.appendChild(overlay);
    initBackdropEvents();

    hoverOutline = mkEl("div", "hover-outline");
    document.body.appendChild(hoverOutline);

    componentTraceEl = mkEl("div", "component-trace");
    document.body.appendChild(componentTraceEl);

    // Toolbar container — direction set by positionFab
    toolbarContainer = mkEl("div", "toolbar-container");

    // Toolbar pill
    toolbar = mkEl("div", "toolbar");

    toolBtnSelect = makeToolBtn("select-element", ICONS.crosshair, tipHtml("Select element", [KEY_MOD, KEY_SHIFT, "F"]));
    toolBtnScreenshot = makeToolBtn("screenshot", ICONS.screenshot, tipHtml("Screenshot", [KEY_MOD, KEY_SHIFT, "S"]));
    toolBtnPageComment = makeToolBtn("page-comment", ICONS.pageComment, tipHtml("Page comment", [KEY_MOD, KEY_SHIFT, "P"]));
    toolBtnComments = makeToolBtn("show-comments", ICONS.chat, tipHtml("Comments", [KEY_MOD, KEY_SHIFT, "C"]));
    toolBtnSubmit = makeToolBtn("submit", ICONS.send, "Submit feedback");
    toolBtnApprove = makeToolBtn("approve", ICONS.check, "All good");
    toolBtnCancel = makeToolBtn("cancel-session", ICONS.cancel, "Cancel session");
    toolBtnCancel.style.display = "none"; // only visible during active wait
    var sep = mkEl("div", "separator");
    toolBtnHide = makeToolBtn("hide", ICONS.eyeOff, tipHtml("Hide", [KEY_MOD, KEY_SHIFT, "."]));

    toolbar.appendChild(toolBtnSelect);
    toolbar.appendChild(toolBtnScreenshot);
    toolbar.appendChild(toolBtnPageComment);
    toolbar.appendChild(toolBtnComments);
    toolbar.appendChild(toolBtnSubmit);
    toolbar.appendChild(toolBtnApprove);
    toolbar.appendChild(toolBtnCancel);
    toolbar.appendChild(sep);
    toolbar.appendChild(toolBtnHide);

    // Screenshot selection rectangle (hidden until drag)
    screenshotRect = mkEl("div", "screenshot-rect");
    document.body.appendChild(screenshotRect);
    toolbarContainer.appendChild(toolbar);

    // FAB
    fab = mkEl("button", "fab");
    attachTooltip(fab, tipHtml("Veld Feedback", [KEY_MOD, KEY_SHIFT, "V"]));
    fab.innerHTML = ICONS.logo;
    fabBadge = mkEl("span", "badge badge-hidden");
    fab.appendChild(fabBadge);
    fab.addEventListener("click", function () {
      if (fab._wasDragged) { fab._wasDragged = false; return; }
      toggleToolbar();
    });
    toolbarContainer.appendChild(fab);

    document.body.appendChild(toolbarContainer);
    initDrag();

    // Panel
    panel = mkEl("div", "panel");
    var panelHead = mkEl("div", "panel-head");
    panelHead.appendChild(mkEl("span", null, "Feedback"));
    var closeBtn = mkEl("button", "panel-close");
    closeBtn.innerHTML = "&times;";
    closeBtn.addEventListener("click", togglePanel);
    panelHead.appendChild(closeBtn);
    panel.appendChild(panelHead);

    panelBody = mkEl("div", "panel-body");
    panel.appendChild(panelBody);

    panelFooter = mkEl("div", "panel-footer");
    var approveBtn = mkEl("button", "btn btn-secondary", "All Good \u2714");
    approveBtn.addEventListener("click", approveAll);
    panelFooter.appendChild(approveBtn);
    var submitBtn = mkEl("button", "btn btn-primary", "Submit Feedback");
    submitBtn.addEventListener("click", submitAll);
    panelFooter.appendChild(submitBtn);
    panel.appendChild(panelFooter);

    document.body.appendChild(panel);
  }

  // ---------- custom tooltip ------------------------------------------------

  var tooltip = null;

  function initTooltip() {
    tooltip = mkEl("div", "tooltip");
    document.body.appendChild(tooltip);
  }

  function showTooltip(anchor, html) {
    tooltip.innerHTML = html;
    tooltip.style.display = "block";
    var r = anchor.getBoundingClientRect();
    var tw = tooltip.offsetWidth;
    var th = tooltip.offsetHeight;
    var gap = 8;
    // Prefer above
    var top = r.top + window.scrollY - th - gap;
    if (top < window.scrollY + 4) {
      top = r.bottom + window.scrollY + gap; // flip below
    }
    var left = r.left + window.scrollX + r.width / 2 - tw / 2;
    left = Math.max(window.scrollX + 4, Math.min(window.scrollX + window.innerWidth - tw - 4, left));
    tooltip.style.top = top + "px";
    tooltip.style.left = left + "px";
  }

  function hideTooltip() {
    tooltip.style.display = "none";
  }

  /** Build tooltip HTML: label with optional kbd shortcut */
  /** Build tooltip HTML. `keys` is an array of individual key labels, e.g. [KEY_MOD, KEY_SHIFT, "F"]. */
  function tipHtml(label, keys) {
    var h = label;
    if (keys && keys.length) {
      h += ' <span class="' + PREFIX + 'kbd-group">';
      for (var i = 0; i < keys.length; i++) {
        h += '<kbd class="' + PREFIX + 'kbd">' + keys[i] + '</kbd>';
      }
      h += '</span>';
    }
    return h;
  }

  function attachTooltip(el, html) {
    el.addEventListener("mouseenter", function () { showTooltip(el, html); });
    el.addEventListener("mouseleave", hideTooltip);
    el.addEventListener("mousedown", hideTooltip);
  }

  // ---------- tool buttons --------------------------------------------------

  function makeToolBtn(action, iconSvg, title) {
    var btn = mkEl("button", "tool-btn");
    btn.dataset.action = action;
    btn.innerHTML = iconSvg;
    attachTooltip(btn, title);
    btn.addEventListener("click", function (e) {
      e.stopPropagation();
      handleToolAction(action);
    });
    return btn;
  }

  function handleToolAction(action) {
    if (action === "select-element") {
      setMode(__veld_activeMode === "select-element" ? null : "select-element");
    } else if (action === "screenshot") {
      setMode(__veld_activeMode === "screenshot" ? null : "screenshot");
    } else if (action === "page-comment") {
      togglePageComment();
    } else if (action === "submit") {
      showSubmitConfirm();
    } else if (action === "approve") {
      showApproveConfirm();
    } else if (action === "cancel-session") {
      cancelFeedbackSession();
    } else if (action === "show-comments") {
      togglePanel();
    } else if (action === "hide") {
      hideOverlay();
    }
  }

  // ---------- FAB dragging ------------------------------------------------

  var FAB_MARGIN = 16; // minimum distance from viewport edge

  function initDrag() {
    var startX, startY, origX, origY, dragging = false, moved = false;

    fab.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      dragging = true; moved = false;
      startX = e.clientX; startY = e.clientY;
      var rect = fab.getBoundingClientRect();
      origX = rect.left + rect.width / 2;
      origY = rect.top + rect.height / 2;
      e.preventDefault();
    });

    document.addEventListener("mousemove", function (e) {
      if (!dragging) return;
      var dx = e.clientX - startX, dy = e.clientY - startY;
      if (!moved && Math.abs(dx) < 4 && Math.abs(dy) < 4) return;
      moved = true;
      var nx = origX + dx, ny = origY + dy;
      nx = Math.max(20 + FAB_MARGIN, Math.min(window.innerWidth - 20 - FAB_MARGIN, nx));
      ny = Math.max(20 + FAB_MARGIN, Math.min(window.innerHeight - 20 - FAB_MARGIN, ny));
      positionFab(nx, ny, false);
    });

    document.addEventListener("mouseup", function () {
      if (!dragging) return;
      dragging = false;
      if (moved) {
        fab._wasDragged = true;
        setTimeout(function () { fab._wasDragged = false; }, 300);
        var rect = fab.getBoundingClientRect();
        var cx = rect.left + rect.width / 2;
        var cy = rect.top + rect.height / 2;
        saveFabPos(cx, cy);
      }
    });
  }

  function positionFab(cx, cy, animate) {
    var onRight = cx > window.innerWidth / 2;
    toolbarContainer.style.transition = animate ? "all .2s ease" : "none";
    toolbarContainer.style.top = (cy - 20) + "px";

    // Anchor from the correct edge so the toolbar can expand inward without
    // pushing the FAB off-screen.
    // DOM order: [toolbar, fab].
    //   row:         toolbar(left) fab(right) → toolbar extends LEFT from FAB. Good for right side.
    //   row-reverse: fab(left) toolbar(right) → toolbar extends RIGHT from FAB. Good for left side.
    if (onRight) {
      // FAB on right: anchor container's RIGHT edge so toolbar grows leftward.
      toolbarContainer.style.left = "auto";
      toolbarContainer.style.right = (window.innerWidth - cx - 20) + "px";
    } else {
      // FAB on left: anchor container's LEFT edge so toolbar grows rightward.
      toolbarContainer.style.right = "auto";
      toolbarContainer.style.left = (cx - 20) + "px";
    }

    toolbarContainer.classList.toggle(PREFIX + "toolbar-right", onRight);
    toolbarContainer.classList.toggle(PREFIX + "toolbar-left", !onRight);
  }

  function saveFabPos(x, y) {
    try { sessionStorage.setItem("veld-fab-pos", JSON.stringify({ x: x, y: y })); } catch (_) {}
  }

  function restoreFabPos() {
    try {
      var saved = sessionStorage.getItem("veld-fab-pos");
      if (saved) {
        var pos = JSON.parse(saved);
        positionFab(pos.x, pos.y, false);
        return;
      }
    } catch (_) {}
    positionFab(window.innerWidth - 20 - FAB_MARGIN, window.innerHeight - 20 - FAB_MARGIN, false);
  }

  function clampFabToViewport() {
    var rect = fab.getBoundingClientRect();
    var cx = rect.left + rect.width / 2;
    var cy = rect.top + rect.height / 2;
    var clamped = false;
    var maxX = window.innerWidth - 20 - FAB_MARGIN;
    var maxY = window.innerHeight - 20 - FAB_MARGIN;
    var minXY = 20 + FAB_MARGIN;
    if (cx > maxX) { cx = maxX; clamped = true; }
    if (cx < minXY) { cx = minXY; clamped = true; }
    if (cy > maxY) { cy = maxY; clamped = true; }
    if (cy < minXY) { cy = minXY; clamped = true; }
    if (clamped) { positionFab(cx, cy, true); saveFabPos(cx, cy); }
  }

  // ---------- toolbar toggle ----------------------------------------------

  function toggleToolbar() {
    __veld_toolbarOpen = !__veld_toolbarOpen;
    toolbar.classList.toggle(PREFIX + "toolbar-open", __veld_toolbarOpen);
    if (!__veld_toolbarOpen) {
      setMode(null);
    }
  }

  // ---------- badge -------------------------------------------------------

  function updateBadge() {
    var count = __veld_comments.length;
    fabBadge.textContent = count;
    fabBadge.className = count > 0 ? PREFIX + "badge" : PREFIX + "badge " + PREFIX + "badge-hidden";
  }

  // ---------- modes -------------------------------------------------------

  var __veld_captureStream = null; // persistent MediaStream for screenshot mode

  function setMode(mode) {
    // Tear down previous mode
    if (__veld_activeMode === "select-element") {
      overlay.classList.remove(PREFIX + "overlay-active");
      hoverOutline.style.display = "none";
      componentTraceEl.style.display = "none";
      __veld_hoveredEl = null;
      __veld_lockedEl = null;
    }
    if (__veld_activeMode === "screenshot") {
      overlay.classList.remove(PREFIX + "overlay-active");
      overlay.classList.remove(PREFIX + "overlay-crosshair");
      screenshotRect.style.display = "none";
      // Release stream when leaving screenshot mode.
      stopCaptureStream();
    }

    closeActivePopover();
    __veld_activeMode = mode;

    toolBtnSelect.classList.toggle(PREFIX + "tool-active", mode === "select-element");
    toolBtnScreenshot.classList.toggle(PREFIX + "tool-active", mode === "screenshot");

    if (mode === "select-element") {
      overlay.classList.add(PREFIX + "overlay-active");
    }
    if (mode === "screenshot") {
      // Acquire screen capture stream once — subsequent grabs reuse it.
      acquireCaptureStream().then(function () {
        overlay.classList.add(PREFIX + "overlay-active");
        overlay.classList.add(PREFIX + "overlay-crosshair");
      }).catch(function () {
        toast("Screen capture denied", true);
        // Revert mode since we can't capture.
        __veld_activeMode = null;
        toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
      });
    }
  }

  function acquireCaptureStream() {
    if (__veld_captureStream) return Promise.resolve();

    // Show a friendly heads-up before the browser's scary permission dialog.
    var seenKey = "veld-screenshot-disclaimer-seen";
    var seen = false;
    try { seen = sessionStorage.getItem(seenKey) === "1"; } catch (_) {}

    var disclaimerDone = seen
      ? Promise.resolve()
      : new Promise(function (resolve, reject) {
          var backdrop = mkEl("div", "confirm-backdrop");
          var modal = mkEl("div", "confirm-modal");

          var title = mkEl("div", "confirm-message");
          title.style.fontWeight = "600";
          title.style.fontSize = "14px";
          title.style.marginBottom = "8px";
          title.textContent = "Quick heads-up!";
          modal.appendChild(title);

          var msg = mkEl("div", "confirm-message");
          msg.innerHTML = "Your browser is about to ask you to share this tab. "
            + "Don\u2019t worry \u2014 <strong>no one is calling you on Teams.</strong> "
            + "Veld just needs to peek at your tab to capture pixel-perfect screenshots. "
            + "Nothing leaves your machine, pinky promise."
            + "<br><br>"
            + "You\u2019ll see a \u201CSharing this tab\u201D banner \u2014 that\u2019s normal! "
            + "It stays while screenshot mode is active and goes away when you\u2019re done.";
          modal.appendChild(msg);

          var actions = mkEl("div", "confirm-actions");
          var cancelBtn = mkEl("button", "btn btn-secondary", "Nah, skip it");
          cancelBtn.addEventListener("click", function () {
            backdrop.remove();
            reject();
          });
          var goBtn = mkEl("button", "btn btn-primary", "Got it, let\u2019s go!");
          goBtn.addEventListener("click", function () {
            try { sessionStorage.setItem(seenKey, "1"); } catch (_) {}
            backdrop.remove();
            resolve();
          });
          actions.appendChild(cancelBtn);
          actions.appendChild(goBtn);
          modal.appendChild(actions);

          backdrop.appendChild(modal);
          document.body.appendChild(backdrop);
          requestAnimationFrame(function () {
            backdrop.classList.add(PREFIX + "confirm-backdrop-visible");
          });
        });

    return disclaimerDone.then(function () {
      var opts = { video: { displaySurface: "browser" }, preferCurrentTab: true };
      return navigator.mediaDevices.getDisplayMedia(opts).then(function (stream) {
        __veld_captureStream = stream;
        // If the user stops sharing via browser UI, clean up.
        stream.getVideoTracks()[0].addEventListener("ended", function () {
          __veld_captureStream = null;
          if (__veld_activeMode === "screenshot") setMode(null);
        });
      });
    });
  }

  function stopCaptureStream() {
    if (__veld_captureStream) {
      __veld_captureStream.getTracks().forEach(function (t) { t.stop(); });
      __veld_captureStream = null;
    }
  }

  // ---------- hide / show overlay -----------------------------------------

  function hideOverlay() {
    __veld_hidden = true;
    try { sessionStorage.setItem("veld-feedback-hidden", "1"); } catch (_) {}
    toolbarContainer.classList.add(PREFIX + "hidden");
    Object.keys(__veld_pins).forEach(function (id) { __veld_pins[id].classList.add(PREFIX + "hidden"); });
    overlay.classList.remove(PREFIX + "overlay-active");
    hoverOutline.style.display = "none";
    componentTraceEl.style.display = "none";
    setMode(null);
    if (__veld_panelOpen) togglePanel();
  }

  function showOverlay() {
    __veld_hidden = false;
    try { sessionStorage.removeItem("veld-feedback-hidden"); } catch (_) {}
    toolbarContainer.classList.remove(PREFIX + "hidden");
    Object.keys(__veld_pins).forEach(function (id) { __veld_pins[id].classList.remove(PREFIX + "hidden"); });
  }

  // ---------- hover highlight (select-element mode) -----------------------

  /** Temporarily hide the backdrop, find the page element under the cursor, restore backdrop. */
  function elementBelowBackdrop(x, y) {
    overlay.style.display = "none";
    hoverOutline.style.display = "none";
    componentTraceEl.style.display = "none";
    var el = document.elementFromPoint(x, y);
    overlay.style.display = "";
    // Skip our own UI elements
    if (el && isOwnElement(el)) el = null;
    return el;
  }

  function isOwnElement(el) {
    while (el) {
      if (el.className && typeof el.className === "string" && el.className.indexOf(PREFIX) !== -1) return true;
      el = el.parentElement;
    }
    return false;
  }

  function initBackdropEvents() {
    // --- Screenshot drag state ---
    var ssStartX, ssStartY, ssDragging = false;

    overlay.addEventListener("mousemove", function (e) {
      if (__veld_activeMode === "select-element") {
        if (__veld_lockedEl) return;
        var target = elementBelowBackdrop(e.clientX, e.clientY);
        if (!target) {
          hoverOutline.style.display = "none";
          componentTraceEl.style.display = "none";
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

        var trace = getComponentTrace(target);
        if (trace && trace.length) {
          componentTraceEl.textContent = formatTrace(trace);
          componentTraceEl.style.display = "block";
          positionTooltip(componentTraceEl, r);
        } else {
          componentTraceEl.style.display = "none";
        }
      } else if (__veld_activeMode === "screenshot" && ssDragging) {
        var x = Math.min(ssStartX, e.clientX);
        var y = Math.min(ssStartY, e.clientY);
        var w = Math.abs(e.clientX - ssStartX);
        var h = Math.abs(e.clientY - ssStartY);
        screenshotRect.style.display = "block";
        screenshotRect.style.left = (x + window.scrollX) + "px";
        screenshotRect.style.top = (y + window.scrollY) + "px";
        screenshotRect.style.width = w + "px";
        screenshotRect.style.height = h + "px";
      }
    });

    overlay.addEventListener("mousedown", function (e) {
      e.preventDefault();
      e.stopPropagation();
      if (__veld_activeMode === "screenshot") {
        ssDragging = true;
        ssStartX = e.clientX;
        ssStartY = e.clientY;
        screenshotRect.style.display = "none";
      }
    });

    overlay.addEventListener("mouseup", function (e) {
      e.preventDefault();
      e.stopPropagation();
      if (__veld_activeMode === "screenshot" && ssDragging) {
        ssDragging = false;
        var x = Math.min(ssStartX, e.clientX);
        var y = Math.min(ssStartY, e.clientY);
        var w = Math.abs(e.clientX - ssStartX);
        var h = Math.abs(e.clientY - ssStartY);
        screenshotRect.style.display = "none";
        // Ignore tiny drags (accidental clicks)
        if (w > 10 && h > 10) {
          captureScreenshot(x, y, w, h);
        }
      }
    });

    overlay.addEventListener("click", function (e) {
      e.preventDefault();
      e.stopPropagation();
      if (__veld_activeMode === "select-element") {
        var target = __veld_hoveredEl || elementBelowBackdrop(e.clientX, e.clientY);
        if (!target) return;

        var rect = docRect(target);
        var selector = selectorFor(target);
        var tagInfo = target.tagName.toLowerCase();
        if (target.className && typeof target.className === "string") {
          var cls = target.className.trim().split(/\s+/).filter(function (c) { return !c.startsWith(PREFIX); });
          if (cls.length) tagInfo += "." + cls.slice(0, 3).join(".");
        }

        var trace = getComponentTrace(target);
        showCreatePopover(rect, selector, tagInfo, target, trace);
      }
      // Screenshot click is handled by mouseup (drag end)
    });
  }

  // ---------- screenshot capture -------------------------------------------

  function captureScreenshot(viewX, viewY, viewW, viewH) {
    // Hide veld UI so the screenshot is clean.
    var veldEls = document.querySelectorAll(
      "[class^='" + PREFIX + "'], [class*=' " + PREFIX + "']"
    );
    var hiddenEls = [];
    veldEls.forEach(function (el) {
      if (el.style.display !== "none") {
        hiddenEls.push({ el: el, prev: el.style.visibility });
        el.style.visibility = "hidden";
      }
    });

    // Exit screenshot mode (removes backdrop) but keep the stream alive.
    var stream = __veld_captureStream;
    __veld_captureStream = null; // prevent setMode(null) from stopping it
    setMode(null);
    __veld_captureStream = stream; // restore for reuse

    if (!stream) {
      restoreVeldUI(hiddenEls);
      showScreenshotCommentEditor(null, null, viewX, viewY, viewW, viewH);
      return;
    }

    var track = stream.getVideoTracks()[0];

    function grabCleanFrame() {
      var grabber = new ImageCapture(track);
      grabber.grabFrame().then(function (bitmap) {
        restoreVeldUI(hiddenEls);
        cropAndShowEditor(bitmap, viewX, viewY, viewW, viewH);
      }).catch(function () {
        restoreVeldUI(hiddenEls);
        showScreenshotCommentEditor(null, null, viewX, viewY, viewW, viewH);
      });
    }

    // Wait for the UI to fully repaint before capturing: two rAF cycles
    // to flush styles + composite, plus a small timeout as safety margin
    // for slower compositors.
    requestAnimationFrame(function () {
      requestAnimationFrame(function () {
        setTimeout(grabCleanFrame, 50);
      });
    });
  }

  function restoreVeldUI(hiddenEls) {
    hiddenEls.forEach(function (item) {
      item.el.style.visibility = item.prev;
    });
  }

  function cropAndShowEditor(bitmap, viewX, viewY, viewW, viewH) {
    // The captured bitmap may be at native resolution (dpr-scaled).
    var scaleX = bitmap.width / window.innerWidth;
    var scaleY = bitmap.height / window.innerHeight;

    var canvas = document.createElement("canvas");
    canvas.width = Math.round(viewW * scaleX);
    canvas.height = Math.round(viewH * scaleY);
    var ctx = canvas.getContext("2d");

    // Crop: draw the full bitmap offset so only the selected area is visible.
    ctx.drawImage(
      bitmap,
      Math.round(viewX * scaleX), Math.round(viewY * scaleY),
      canvas.width, canvas.height,
      0, 0,
      canvas.width, canvas.height
    );
    bitmap.close();

    canvas.toBlob(function (pngBlob) {
      if (!pngBlob) {
        showScreenshotCommentEditor(null, null, viewX, viewY, viewW, viewH);
        return;
      }
      uploadAndShowEditor(pngBlob, viewX, viewY, viewW, viewH);
    }, "image/png");
  }

  function uploadAndShowEditor(pngBlob, viewX, viewY, viewW, viewH) {
    var screenshotId = "ss_" + Date.now() + "_" + Math.random().toString(36).slice(2, 8);

    fetch(API + "/screenshots/" + screenshotId, {
      method: "POST",
      headers: { "Content-Type": "image/png" },
      body: pngBlob
    }).then(function (res) {
      if (!res.ok) throw new Error("Upload failed: " + res.status);
      showScreenshotCommentEditor(pngBlob, screenshotId, viewX, viewY, viewW, viewH);
    }).catch(function (err) {
      toast("Screenshot upload failed: " + err.message, true);
      // Still show the editor, just without the stored screenshot.
      showScreenshotCommentEditor(null, null, viewX, viewY, viewW, viewH);
    });
  }

  function showScreenshotCommentEditor(pngBlob, screenshotId, viewX, viewY, viewW, viewH) {
    closeActivePopover();

    var pop = mkEl("div", "popover");
    pop._veldType = "screenshot";

    // Screenshot preview (if available)
    var previewUrl = null;
    if (pngBlob) {
      previewUrl = URL.createObjectURL(pngBlob);
      var previewContainer = mkEl("div", "screenshot-preview");
      var previewImg = document.createElement("img");
      previewImg.src = previewUrl;
      previewImg.className = PREFIX + "screenshot-img";
      previewContainer.appendChild(previewImg);
      pop.appendChild(previewContainer);
    }

    // Ensure the Object URL is revoked when the popover is closed by any means
    // (cancel button, save button, Escape key, or another popover opening).
    pop._veldCleanup = function () {
      if (previewUrl) { URL.revokeObjectURL(previewUrl); previewUrl = null; }
    };

    var header = mkEl("div", "popover-header", "Screenshot comment \u2014 " + window.location.pathname);
    pop.appendChild(header);

    var body = mkEl("div", "popover-body");
    var ta = mkEl("textarea", "textarea");
    ta.placeholder = "Describe what you see\u2026";
    body.appendChild(ta);

    var actions = mkEl("div", "popover-actions");
    var cancelBtn = mkEl("button", "btn btn-secondary", "Cancel");
    cancelBtn.addEventListener("click", function () {
      closeActivePopover();
    });
    var saveBtn = mkEl("button", "btn btn-primary", "Save");
    saveBtn.addEventListener("click", function () {
      var text = ta.value.trim();
      if (!text) { ta.focus(); return; }
      if (saveBtn.disabled) return;
      saveBtn.disabled = true;
      saveScreenshotComment(text, viewX, viewY, viewW, viewH, screenshotId, function () {
        saveBtn.disabled = false;
      });
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(saveBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    // Highlight screenshot toolbar button while editor is open.
    toolBtnScreenshot.classList.add(PREFIX + "tool-active");

    document.body.appendChild(pop);
    __veld_activePopover = pop;

    // Position in center of viewport.
    var centerRect = {
      x: window.scrollX + window.innerWidth / 2 - 160,
      y: window.scrollY + window.innerHeight / 3,
      width: 320,
      height: 0
    };
    positionPopover(pop, centerRect);
    ta.focus();
  }

  function saveScreenshotComment(comment, viewX, viewY, viewW, viewH, screenshotId, onError) {
    var payload = {
      page_url: window.location.pathname,
      element_selector: null,
      comment: comment,
      position: {
        x: viewX + window.scrollX,
        y: viewY + window.scrollY,
        width: viewW,
        height: viewH
      },
      component_trace: null,
      screenshot: screenshotId || null,
      viewport_width: window.innerWidth,
      viewport_height: window.innerHeight
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
        toast("Screenshot comment saved");
      })
      .catch(function (err) {
        if (onError) onError();
        toast("Failed to save: " + err.message, true);
      });
  }


  // ---------- popover positioning (viewport-aware) ------------------------

  function positionPopover(pop, anchorRect) {
    // Try below the element first
    var popWidth = 320;
    var popHeight = 260; // estimated
    var gap = 10;
    var margin = 16;

    var topBelow = anchorRect.y + anchorRect.height + gap;
    var topAbove = anchorRect.y - popHeight - gap;

    // Vertical: prefer below, flip above if it would overflow
    var top;
    if (topBelow + popHeight > window.scrollY + window.innerHeight - margin && topAbove > window.scrollY + margin) {
      top = topAbove;
    } else {
      top = topBelow;
    }

    // Horizontal: center on anchor, clamp to viewport
    var left = anchorRect.x + anchorRect.width / 2 - popWidth / 2;
    var maxLeft = window.scrollX + window.innerWidth - popWidth - margin;
    var minLeft = window.scrollX + margin;
    left = Math.max(minLeft, Math.min(maxLeft, left));

    pop.style.top = top + "px";
    pop.style.left = left + "px";
  }

  function positionTooltip(el, viewportRect) {
    var gap = 6;
    var margin = 8;
    var aboveY = viewportRect.top + window.scrollY - el.offsetHeight - gap;
    var belowY = viewportRect.top + window.scrollY + viewportRect.height + gap;
    // Prefer above, flip below if it would go off-screen
    if (aboveY < window.scrollY + margin) {
      el.style.top = belowY + "px";
    } else {
      el.style.top = aboveY + "px";
    }
    var left = viewportRect.left + window.scrollX;
    var maxLeft = window.scrollX + window.innerWidth - el.offsetWidth - margin;
    el.style.left = Math.max(window.scrollX + margin, Math.min(maxLeft, left)) + "px";
  }

  function closeActivePopover() {
    if (__veld_activePopover) {
      // Run cleanup callback (e.g. revoke Object URLs) before removing.
      if (typeof __veld_activePopover._veldCleanup === "function") {
        __veld_activePopover._veldCleanup();
      }
      __veld_activePopover.remove();
      __veld_activePopover = null;
    }
    // Unlock the element highlight
    if (__veld_lockedEl) {
      __veld_lockedEl = null;
      hoverOutline.style.display = "none";
      componentTraceEl.style.display = "none";
    }
    // Unhighlight tool buttons
    if (toolBtnPageComment) toolBtnPageComment.classList.remove(PREFIX + "tool-active");
    if (toolBtnScreenshot) toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
  }

  // ---------- create popover ----------------------------------------------

  function showCreatePopover(rect, selector, tagInfo, targetEl, trace) {
    closeActivePopover();

    // Lock the element highlight while the popover is open
    __veld_lockedEl = targetEl;

    var pop = mkEl("div", "popover");

    if (trace && trace.length) {
      var header = mkEl("div", "popover-header", formatTrace(trace));
      pop.appendChild(header);
    }

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
      if (saveBtn.disabled) return;
      saveBtn.disabled = true;
      saveComment(selector, text, rect, trace, function () { saveBtn.disabled = false; });
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(saveBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    document.body.appendChild(pop);
    __veld_activePopover = pop;
    positionPopover(pop, rect);
    ta.focus();
  }

  // ---------- page comment popover -----------------------------------------

  function togglePageComment() {
    if (__veld_activePopover && __veld_activePopover._veldType === "page-comment") {
      closeActivePopover();
      return;
    }
    showPageCommentPopover();
  }

  function showPageCommentPopover() {
    setMode(null); // deactivate any active mode
    closeActivePopover();

    var pop = mkEl("div", "popover");
    pop._veldType = "page-comment";

    var header = mkEl("div", "popover-header", "Page comment \u2014 " + window.location.pathname);
    pop.appendChild(header);

    var body = mkEl("div", "popover-body");
    var ta = mkEl("textarea", "textarea");
    ta.placeholder = "Add a comment about this page\u2026";
    body.appendChild(ta);

    var actions = mkEl("div", "popover-actions");
    var cancelBtn = mkEl("button", "btn btn-secondary", "Cancel");
    cancelBtn.addEventListener("click", function () { closeActivePopover(); });
    var saveBtn = mkEl("button", "btn btn-primary", "Save");
    saveBtn.addEventListener("click", function () {
      var text = ta.value.trim();
      if (!text) { ta.focus(); return; }
      if (saveBtn.disabled) return;
      saveBtn.disabled = true;
      saveComment(null, text, null, null, function () { saveBtn.disabled = false; });
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(saveBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    document.body.appendChild(pop);
    __veld_activePopover = pop;
    toolBtnPageComment.classList.add(PREFIX + "tool-active");

    // Position in center of viewport
    var centerRect = {
      x: window.scrollX + window.innerWidth / 2 - 160,
      y: window.scrollY + window.innerHeight / 3,
      width: 320,
      height: 0
    };
    positionPopover(pop, centerRect);
    ta.focus();
  }

  // ---------- save comment ------------------------------------------------

  function saveComment(selector, comment, position, trace, onError) {
    var payload = {
      page_url: window.location.pathname,
      element_selector: selector || null,
      comment: comment,
      position: position ? { x: position.x, y: position.y, width: position.width, height: position.height } : null,
      component_trace: trace || null,
      viewport_width: window.innerWidth,
      viewport_height: window.innerHeight
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
        if (onError) onError();
        toast("Failed to save comment: " + err.message, true);
      });
  }

  // ---------- pins --------------------------------------------------------

  var __veld_pins = {};

  function addPin(comment) {
    removePin(comment.id);
    var idx = __veld_comments.indexOf(comment);
    var num = idx >= 0 ? idx + 1 : Object.keys(__veld_pins).length + 1;

    var pin = mkEl("div", "pin", String(num));
    pin.dataset.commentId = comment.id;
    if (comment.position) {
      pin.style.top = (comment.position.y - 12) + "px";
      pin.style.left = (comment.position.x + comment.position.width - 12) + "px";
    }
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
    Object.keys(__veld_pins).forEach(function (id) { __veld_pins[id].remove(); });
    __veld_pins = {};
    __veld_comments.forEach(addPin);
  }

  function renderPinsForCurrentPage() {
    Object.keys(__veld_pins).forEach(function (id) { __veld_pins[id].remove(); });
    __veld_pins = {};
    currentPageComments().forEach(addPin);
  }

  function repositionPins() {
    __veld_comments.forEach(function (c) {
      var pin = __veld_pins[c.id];
      if (!pin) return;
      if (!c.element_selector) return;
      try {
        var el = document.querySelector(c.element_selector);
        if (el) {
          var r = docRect(el);
          c.position = { x: r.x, y: r.y, width: r.width, height: r.height };
          pin.style.top = (r.y - 12) + "px";
          pin.style.left = (r.x + r.width - 12) + "px";
        }
      } catch (_) {}
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
    var pinRect = docRect(pinEl);

    var headerText = comment.element_selector || "Comment";
    if (comment.component_trace && comment.component_trace.length) {
      headerText += " \u2014 " + formatTrace(comment.component_trace);
    }
    var header = mkEl("div", "popover-header", headerText);
    pop.appendChild(header);

    var body = mkEl("div", "popover-body");

    var textEl = mkEl("div", "comment-text", comment.comment);
    body.appendChild(textEl);

    var actions = mkEl("div", "popover-actions");
    var editBtn = mkEl("button", "btn btn-secondary btn-sm", "Edit");
    var delBtn = mkEl("button", "btn btn-danger btn-sm", "Delete");
    var cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Close");
    cancelBtn.addEventListener("click", function () { closeActivePopover(); });

    editBtn.addEventListener("click", function () {
      var ta = mkEl("textarea", "textarea");
      ta.value = comment.comment;
      textEl.replaceWith(ta);
      ta.focus();
      editBtn.style.display = "none";
      var saveBtn = mkEl("button", "btn btn-primary btn-sm", "Save");
      saveBtn.addEventListener("click", function () {
        var newText = ta.value.trim();
        if (!newText) { ta.focus(); return; }
        if (saveBtn.disabled) return;
        saveBtn.disabled = true;
        var updated = Object.assign({}, comment, { comment: newText });
        api("PUT", "/comments/" + encodeURIComponent(comment.id), updated)
          .then(function () {
            comment.comment = newText;
            renderPanel();
            closeActivePopover();
            toast("Comment updated");
          })
          .catch(function (err) { saveBtn.disabled = false; toast("Update failed: " + err.message, true); });
      });
      actions.insertBefore(saveBtn, delBtn);
    });

    delBtn.addEventListener("click", function () {
      if (!confirm("Delete this comment?")) return;
      if (delBtn.disabled) return;
      delBtn.disabled = true;
      api("DELETE", "/comments/" + encodeURIComponent(comment.id))
        .then(function () {
          __veld_comments = __veld_comments.filter(function (c) { return c.id !== comment.id; });
          removePin(comment.id);
          updateBadge();
          renderPanel();
          closeActivePopover();
          toast("Comment deleted");
        })
        .catch(function (err) { delBtn.disabled = false; toast("Delete failed: " + err.message, true); });
    });

    actions.appendChild(editBtn);
    actions.appendChild(delBtn);
    actions.appendChild(cancelBtn);
    body.appendChild(actions);
    pop.appendChild(body);

    document.body.appendChild(pop);
    __veld_activePopover = pop;
    positionPopover(pop, pinRect);
  }

  // ---------- panel -------------------------------------------------------

  function togglePanel() {
    __veld_panelOpen = !__veld_panelOpen;
    if (__veld_panelOpen) {
      panel.classList.add(PREFIX + "panel-open");
      toolBtnComments.classList.add(PREFIX + "tool-active");
      renderPanel();
    } else {
      panel.classList.remove(PREFIX + "panel-open");
      toolBtnComments.classList.remove(PREFIX + "tool-active");
    }
  }

  function renderPanel() {
    panelBody.innerHTML = "";

    // Always show footer (for "All Good" button), toggle submit button visibility
    var submitBtn = panelFooter.querySelector("." + PREFIX + "btn-primary");
    if (submitBtn) submitBtn.style.display = __veld_comments.length > 0 ? "" : "none";

    if (__veld_comments.length === 0) {
      panelBody.appendChild(mkEl("div", "panel-empty", "No comments yet. Use the toolbar to add feedback, or approve with \"All Good\"."));
      return;
    }

    // Group comments by page
    var currentPath = window.location.pathname;
    var pages = {};
    var pageOrder = [];
    __veld_comments.forEach(function (c) {
      var p = (c.page_url || "").split("?")[0] || "/";
      if (!pages[p]) { pages[p] = []; pageOrder.push(p); }
      pages[p].push(c);
    });

    // Sort: current page first, then alphabetical
    pageOrder.sort(function (a, b) {
      if (a === currentPath) return -1;
      if (b === currentPath) return 1;
      return a.localeCompare(b);
    });

    var globalIdx = 0;
    pageOrder.forEach(function (pagePath) {
      var comments = pages[pagePath];
      var isCurrent = pagePath === currentPath;

      // Page group header
      var groupHeader = mkEl("div", "panel-page-header");
      var pathLabel = mkEl("span", "panel-page-path", pagePath);
      groupHeader.appendChild(pathLabel);
      if (isCurrent) {
        var badge = mkEl("span", "panel-page-badge", "this page");
        groupHeader.appendChild(badge);
      } else {
        var goBtn = mkEl("button", "btn btn-secondary btn-sm", "Go to page");
        goBtn.addEventListener("click", function () { window.location.href = pagePath; });
        groupHeader.appendChild(goBtn);
      }
      panelBody.appendChild(groupHeader);

      comments.forEach(function (c) {
        globalIdx++;
        var idx = globalIdx; // capture current value for closures below
        var item = mkEl("div", "panel-item");

        var title = (c.component_trace && c.component_trace.length)
          ? formatTrace(c.component_trace)
          : (c.screenshot ? "Screenshot" : (c.element_selector || "Comment"));
        var sel = mkEl("div", "panel-item-selector", idx + ". " + title);
        item.appendChild(sel);

        if (c.screenshot) {
          var thumbContainer = mkEl("div", "panel-item-screenshot");
          var thumb = document.createElement("img");
          thumb.className = PREFIX + "panel-item-screenshot-img";
          // Load via our API — the screenshot ID is the filename without extension
          // screenshot field is now just the ID (e.g. "ss_123_abc") or legacy path.
          var ssId = c.screenshot.split("/").pop().replace(".png", "");
          thumb.src = "/__veld__/feedback/api/screenshots/" + ssId;
          thumbContainer.appendChild(thumb);
          item.appendChild(thumbContainer);
        }

        var txt = mkEl("div", "panel-item-comment", c.comment);
        item.appendChild(txt);

        var acts = mkEl("div", "panel-item-actions");

        var editBtn = mkEl("button", "btn btn-secondary btn-sm", "Edit");
        editBtn.addEventListener("click", function () { startInlineEdit(c, item, idx); });
        acts.appendChild(editBtn);

        var delBtn = mkEl("button", "btn btn-danger btn-sm", "Delete");
        delBtn.addEventListener("click", function () {
          if (!confirm("Delete this comment?")) return;
          if (delBtn.disabled) return;
          delBtn.disabled = true;
          api("DELETE", "/comments/" + encodeURIComponent(c.id))
            .then(function () {
              __veld_comments = __veld_comments.filter(function (x) { return x.id !== c.id; });
              removePin(c.id);
              updateBadge();
              renderPanel();
              toast("Comment deleted");
            })
            .catch(function (err) { delBtn.disabled = false; toast("Delete failed: " + err.message, true); });
        });
        acts.appendChild(delBtn);

        item.appendChild(acts);
        panelBody.appendChild(item);
      });
    });
  }

  function startInlineEdit(comment, itemEl, idx) {
    itemEl.innerHTML = "";

    var editTitle = (comment.component_trace && comment.component_trace.length)
      ? formatTrace(comment.component_trace)
      : (comment.element_selector || "Comment");
    var sel = mkEl("div", "panel-item-selector", idx + ". " + editTitle);
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
      if (saveBtn.disabled) return;
      saveBtn.disabled = true;
      var updated = Object.assign({}, comment, { comment: newText });
      api("PUT", "/comments/" + encodeURIComponent(comment.id), updated)
        .then(function () {
          comment.comment = newText;
          renderPanel();
          toast("Comment updated");
        })
        .catch(function (err) { saveBtn.disabled = false; toast("Update failed: " + err.message, true); });
    });
    acts.appendChild(saveBtn);

    var cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Cancel");
    cancelBtn.addEventListener("click", function () { renderPanel(); });
    acts.appendChild(cancelBtn);

    itemEl.appendChild(acts);
    ta.focus();
  }

  // ---------- submit all --------------------------------------------------

  /** Show a centered confirm modal with backdrop. Returns the backdrop element (stored as __veld_activePopover). */
  function showCenteredConfirm(message, confirmLabel, confirmClass, onConfirm) {
    closeActivePopover();
    setMode(null);

    var backdrop = mkEl("div", "confirm-backdrop");
    var modal = mkEl("div", "confirm-modal");

    var msg = mkEl("div", "confirm-message", message);
    modal.appendChild(msg);

    var actions = mkEl("div", "confirm-actions");
    var cancelBtn = mkEl("button", "btn btn-secondary", "Cancel");
    cancelBtn.addEventListener("click", function () { closeActivePopover(); });
    var confirmBtn = mkEl("button", "btn " + confirmClass, confirmLabel);
    confirmBtn.addEventListener("click", function () {
      if (confirmBtn.disabled) return;
      confirmBtn.disabled = true;
      closeActivePopover();
      onConfirm();
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(confirmBtn);
    modal.appendChild(actions);

    backdrop.appendChild(modal);
    backdrop.addEventListener("click", function (e) {
      if (e.target === backdrop) closeActivePopover();
    });
    document.body.appendChild(backdrop);
    __veld_activePopover = backdrop;

    requestAnimationFrame(function () {
      backdrop.classList.add(PREFIX + "confirm-backdrop-visible");
    });
  }

  function showSubmitConfirm() {
    if (__veld_comments.length === 0) {
      toast("No comments to submit", true);
      return;
    }
    showCenteredConfirm(
      "Submit " + __veld_comments.length + " comment(s) across all pages?",
      "Submit", "btn-primary", submitAll
    );
  }

  function showApproveConfirm() {
    showCenteredConfirm(
      "Approve without feedback? This signals to the waiting agent that everything looks good.",
      "All Good \u2714", "btn-primary", approveAll
    );
  }

  function approveAll() {
    if (__veld_submitting) return;
    __veld_submitting = true;
    // Submit an empty batch — signals "all good, no feedback needed"
    api("POST", "/submit")
      .then(function () {
        __veld_submitting = false;
        __veld_comments = [];
        renderPinsForCurrentPage();
        updateBadge();
        renderPanel();
        toast("Approved \u2014 all good!");
        if (__veld_panelOpen) togglePanel();
        if (__veld_toolbarOpen) toggleToolbar();
        __veld_waitActive = false;
        __veld_waitId = null;
        onWaitEnded();
      })
      .catch(function (err) { __veld_submitting = false; toast("Approve failed: " + err.message, true); });
  }

  var __veld_submitting = false;
  function submitAll() {
    if (__veld_submitting) return;
    __veld_submitting = true;
    api("POST", "/submit")
      .then(function () {
        __veld_submitting = false;
        __veld_comments = [];
        renderPinsForCurrentPage();
        updateBadge();
        renderPanel();
        toast("Feedback submitted!");
        if (__veld_panelOpen) togglePanel();
        if (__veld_toolbarOpen) toggleToolbar();
        __veld_waitActive = false;
        __veld_waitId = null;
        onWaitEnded();
      })
      .catch(function (err) { __veld_submitting = false; toast("Submit failed: " + err.message, true); });
  }

  // ---------- keyboard shortcuts ------------------------------------------
  // Cmd+Shift on Mac, Ctrl+Shift on Windows/Linux — standard modifier combo.

  function onKeyDown(e) {
    var mod = modKey(e) && e.shiftKey;

    // Mod+Shift+V: toggle toolbar (or bring back from hidden)
    if (mod && e.code === "KeyV") {
      e.preventDefault();
      if (__veld_hidden) { showOverlay(); return; }
      toggleToolbar();
      return;
    }

    // Mod+Shift+.: toggle overlay visibility
    if (mod && e.code === "Period") {
      e.preventDefault();
      if (__veld_hidden) { showOverlay(); } else { hideOverlay(); }
      return;
    }

    if (__veld_hidden) return;

    // Mod+Shift+F: select element mode
    if (mod && e.code === "KeyF") {
      e.preventDefault();
      if (!__veld_toolbarOpen) toggleToolbar();
      setMode(__veld_activeMode === "select-element" ? null : "select-element");
      return;
    }

    // Mod+Shift+S: screenshot mode
    if (mod && e.code === "KeyS") {
      e.preventDefault();
      if (!__veld_toolbarOpen) toggleToolbar();
      setMode(__veld_activeMode === "screenshot" ? null : "screenshot");
      return;
    }

    // Mod+Shift+P: page comment
    if (mod && e.code === "KeyP") {
      e.preventDefault();
      if (!__veld_toolbarOpen) toggleToolbar();
      togglePageComment();
      return;
    }

    // Mod+Shift+C: toggle comments panel
    if (mod && e.code === "KeyC") {
      e.preventDefault();
      togglePanel();
      return;
    }

    // Escape: cascading dismiss
    if (e.key === "Escape") {
      if (__veld_activePopover) {
        closeActivePopover();
      } else if (__veld_activeMode) {
        setMode(null);
      } else if (__veld_toolbarOpen) {
        toggleToolbar();
      }
    }
  }

  // ---------- fetch existing comments on load -----------------------------

  function currentPageComments() {
    var path = window.location.pathname;
    return __veld_comments.filter(function (c) {
      return (c.page_url || "").split("?")[0] === path;
    });
  }

  function loadAllComments() {
    api("GET", "/comments")
      .then(function (data) {
        if (Array.isArray(data)) {
          __veld_comments = data;
          updateBadge();
          renderPinsForCurrentPage();
          if (__veld_panelOpen) renderPanel();
        }
      })
      .catch(function () {});
  }

  // ---------- wait-session polling ----------------------------------------

  function pollWaitStatus() {
    api("GET", "/wait-status")
      .then(function (data) {
        var wasWaiting = __veld_waitActive;
        __veld_waitActive = !!(data && data.waiting);
        var newId = (data && data.wait_id) || null;

        if (__veld_waitActive && newId) {
          // New or changed wait session?
          if (newId !== __veld_waitId) {
            __veld_waitId = newId;
            // Only show modal if we haven't acknowledged this specific session.
            var acked = null;
            try { acked = sessionStorage.getItem("veld-wait-acked"); } catch (_) {}
            // Check if we already reloaded for this wait session.
            var reloaded = null;
            try { reloaded = sessionStorage.getItem("veld-wait-reload"); } catch (_) {}
            if (reloaded === newId) {
              // Post-reload: show the modal now.
              try { sessionStorage.removeItem("veld-wait-reload"); } catch (_) {}
              onWaitStarted();
            } else if (acked !== newId) {
              // First detection: hard-reload so reviewer sees fresh page state.
              try { sessionStorage.setItem("veld-wait-reload", newId); } catch (_) {}
              location.reload();
              return;
            } else {
              // Already acked — still pulse FAB + show cancel, but no modal.
              if (fab) fab.classList.add(PREFIX + "fab-pulse");
              if (toolBtnCancel) toolBtnCancel.style.display = "";
            }
          }
        } else if (!__veld_waitActive && wasWaiting) {
          __veld_waitId = null;
          try { sessionStorage.removeItem("veld-wait-acked"); } catch (_) {}
          onWaitEnded();
        }
      })
      .catch(function () {});
  }

  function onWaitStarted() {
    // Unhide overlay if it was hidden.
    if (__veld_hidden) showOverlay();
    // Show the notification modal.
    showWaitModal();
    // Fire browser notification.
    sendBrowserNotification();
    // Pulse the FAB to attract attention.
    if (fab) fab.classList.add(PREFIX + "fab-pulse");
    // Show cancel button in toolbar.
    if (toolBtnCancel) toolBtnCancel.style.display = "";
  }

  function onWaitEnded() {
    dismissWaitModal();
    if (fab) fab.classList.remove(PREFIX + "fab-pulse");
    if (toolBtnCancel) toolBtnCancel.style.display = "none";
    __veld_notificationSent = false;
    // Release screen capture stream — session is over.
    stopCaptureStream();
  }

  function sendBrowserNotification() {
    if (__veld_notificationSent) return;
    if (!("Notification" in window)) return;
    if (Notification.permission === "granted") {
      fireNotification();
    } else if (Notification.permission !== "denied") {
      Notification.requestPermission().then(function (perm) {
        if (perm === "granted") fireNotification();
      });
    }
  }

  function fireNotification() {
    __veld_notificationSent = true;
    var n = new Notification("Veld — Feedback Requested", {
      body: "Someone is waiting for your feedback.",
      icon: "/__veld__/feedback/logo.svg",
      tag: "veld-feedback-wait"
    });
    n.addEventListener("click", function () {
      window.focus();
      n.close();
    });
  }

  // -- wait modal ---------------------------------------------------------

  function showWaitModal() {
    if (__veld_waitModalEl) return;

    var backdrop = mkEl("div", "wait-backdrop");
    var modal = mkEl("div", "wait-modal");

    var icon = mkEl("div", "wait-modal-icon");
    icon.innerHTML = ICONS.logo;
    modal.appendChild(icon);

    var title = mkEl("div", "wait-modal-title", "Feedback Requested");
    modal.appendChild(title);

    var desc = mkEl("div", "wait-modal-desc", "An agent is waiting for your review. Take a look and share your thoughts.");
    modal.appendChild(desc);

    var actions = mkEl("div", "wait-modal-actions");

    var diveBtn = mkEl("button", "btn btn-primary", "Let\u2019s dive in");
    diveBtn.addEventListener("click", function () {
      dismissWaitModal();
      if (!__veld_toolbarOpen) toggleToolbar();
    });

    var cancelBtn = mkEl("button", "btn btn-danger", "Cancel feedback");
    cancelBtn.addEventListener("click", function () {
      dismissWaitModal();
      cancelFeedbackSession();
    });

    actions.appendChild(diveBtn);
    actions.appendChild(cancelBtn);
    modal.appendChild(actions);
    backdrop.appendChild(modal);
    document.body.appendChild(backdrop);
    __veld_waitModalEl = backdrop;

    // Animate in.
    requestAnimationFrame(function () {
      backdrop.classList.add(PREFIX + "wait-backdrop-visible");
    });
  }

  function dismissWaitModal() {
    if (!__veld_waitModalEl) return;
    // Remember that we acknowledged this specific wait session.
    if (__veld_waitId) {
      try { sessionStorage.setItem("veld-wait-acked", __veld_waitId); } catch (_) {}
    }
    __veld_waitModalEl.classList.remove(PREFIX + "wait-backdrop-visible");
    var el = __veld_waitModalEl;
    __veld_waitModalEl = null;
    setTimeout(function () { el.remove(); }, 250);
  }

  function cancelFeedbackSession() {
    api("POST", "/cancel")
      .then(function () {
        toast("Feedback session cancelled.");
        // Immediately update local state — don't wait for next poll.
        __veld_waitActive = false;
        __veld_waitId = null;
        onWaitEnded();
      })
      .catch(function (err) { toast("Cancel failed: " + err.message, true); });
  }

  // ---------- SPA navigation detection ------------------------------------

  var __veld_lastPathname = window.location.pathname;

  function onNavigate() {
    var newPath = window.location.pathname;
    if (newPath !== __veld_lastPathname) {
      __veld_lastPathname = newPath;
      renderPinsForCurrentPage();
      if (__veld_panelOpen) renderPanel();
    }
  }

  var origPushState = history.pushState;
  var origReplaceState = history.replaceState;
  history.pushState = function () {
    origPushState.apply(this, arguments);
    try { onNavigate(); } catch (_) {}
  };
  history.replaceState = function () {
    origReplaceState.apply(this, arguments);
    try { onNavigate(); } catch (_) {}
  };

  // ---------- init --------------------------------------------------------

  function init() {
    try {
      if (sessionStorage.getItem("veld-feedback-hidden") === "1") {
        __veld_hidden = true;
      }
    } catch (_) {}

    buildDOM();
    updateBadge();
    restoreFabPos();

    if (__veld_hidden) {
      toolbarContainer.classList.add(PREFIX + "hidden");
    }

    document.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", scheduleReposition, true);
    window.addEventListener("resize", function () {
      scheduleReposition();
      clampFabToViewport();
    });
    window.addEventListener("popstate", onNavigate);

    loadAllComments();

    // Poll for active wait sessions every 3 seconds.
    pollWaitStatus();
    setInterval(pollWaitStatus, 3000);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
