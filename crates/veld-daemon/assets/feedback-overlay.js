// ---------------------------------------------------------------------------
// Veld Feedback Overlay — Thread-based bidirectional conversation system
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
    cancel: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>',
    robot: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="3" y="4" width="18" height="14" rx="2"/><circle cx="9" cy="11" r="1.5" fill="currentColor" stroke="none"/><circle cx="15" cy="11" r="1.5" fill="currentColor" stroke="none"/><path d="M12 1v3M8 21h8"/></svg>',
    resolve: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="16 9 10.5 15 8 12.5"/></svg>'
  };

  // ---------- state -------------------------------------------------------

  var __veld_threads = [];           // all threads across all pages
  var __veld_lastEventSeq = 0;       // sequence cursor for event polling
  var __veld_lastSeenAt = {};  // threadId -> last seq human has seen
  var __veld_agentListening = false;
  var __veld_panelOpen = false;
  var __veld_panelTab = "active";    // "active" | "resolved"
  var __veld_activePopover = null;
  var __veld_activeMode = null;      // null | 'select-element' | 'screenshot'
  var __veld_hoveredEl = null;
  var __veld_lockedEl = null;
  var __veld_toolbarOpen = false;
  var __veld_hidden = false;
  var __veld_expandedThreadId = null;
  var __veld_pins = {};              // threadId -> pin DOM element

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

  /** Wire Cmd+Enter (Mac) / Ctrl+Enter (others) on a textarea to click a button. */
  function submitOnModEnter(textarea, btn) {
    textarea.addEventListener("keydown", function (e) {
      if (e.key === "Enter" && modKey(e)) {
        e.preventDefault();
        btn.click();
      }
    });
  }

  /** Build a button label with a kbd shortcut hint, e.g. "Send ⌘↵" */
  var SUBMIT_HINT = IS_MAC ? " \u2318\u21A9" : " Ctrl\u21A9";

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
  var toolBtnSelect, toolBtnScreenshot, toolBtnPageComment, toolBtnComments, toolBtnHide;
  var listeningModule;
  var overlay, hoverOutline, componentTraceEl;
  var screenshotRect;
  var panel, panelBody, panelHeadTitle, segControl, panelBackBtn, markReadBtn;
  var segBtnActive, segBtnResolved;

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
    toolBtnComments = makeToolBtn("show-comments", ICONS.chat, tipHtml("Threads", [KEY_MOD, KEY_SHIFT, "C"]));

    toolbar.appendChild(toolBtnSelect);
    toolbar.appendChild(toolBtnScreenshot);
    toolbar.appendChild(toolBtnPageComment);
    toolbar.appendChild(toolBtnComments);

    // Listening section — completely hidden when not listening
    listeningModule = mkEl("div", "listening");

    var listenSep = mkEl("div", "separator");
    listeningModule.appendChild(listenSep);

    // Pulsing dot + "All Good" button
    var listenDot = mkEl("span", "listening-dot");
    attachTooltip(listenDot, "Agent is listening");
    listeningModule.appendChild(listenDot);

    var allGoodBtn = mkEl("button", "listening-allgood", "All Good");
    allGoodBtn.addEventListener("click", function (e) {
      e.stopPropagation();
      sendAllGood();
    });
    listeningModule.appendChild(allGoodBtn);
    toolbar.appendChild(listeningModule);

    // Separator before hide
    var sep2 = mkEl("div", "separator");
    toolbar.appendChild(sep2);

    toolBtnHide = makeToolBtn("hide", ICONS.eyeOff, tipHtml("Hide", [KEY_MOD, KEY_SHIFT, "."]));
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
    panelBackBtn = mkEl("button", "panel-back-btn");
    panelBackBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><polyline points="15 18 9 12 15 6"/></svg>';
    panelBackBtn.style.display = "none";
    panelBackBtn.addEventListener("click", function (e) {
      e.stopPropagation();
      showThreadList();
    });
    panelHead.appendChild(panelBackBtn);
    panelHeadTitle = mkEl("span", "panel-head-title", "Threads");
    panelHead.appendChild(panelHeadTitle);

    // Segmented control
    segControl = mkEl("div", "segmented");
    segBtnActive = mkEl("button", "segmented-btn segmented-btn-active", "Active");
    segBtnActive.addEventListener("click", function () {
      __veld_panelTab = "active";
      renderPanel();
    });
    segBtnResolved = mkEl("button", "segmented-btn", "Resolved");
    segBtnResolved.addEventListener("click", function () {
      __veld_panelTab = "resolved";
      renderPanel();
    });
    segControl.appendChild(segBtnActive);
    segControl.appendChild(segBtnResolved);
    panelHead.appendChild(segControl);

    // Mark all as read
    markReadBtn = mkEl("button", "panel-mark-read");
    markReadBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 7 9.5 17 6 13"/><polyline points="22 7 13.5 17"/></svg>';
    markReadBtn.title = "Mark all as read";
    markReadBtn.style.display = "none";
    markReadBtn.addEventListener("click", function (e) {
      e.stopPropagation();
      markAllRead();
    });
    panelHead.appendChild(markReadBtn);

    var closeBtn = mkEl("button", "panel-close");
    closeBtn.innerHTML = "&times;";
    closeBtn.addEventListener("click", togglePanel);
    panelHead.appendChild(closeBtn);
    panel.appendChild(panelHead);

    panelBody = mkEl("div", "panel-body");
    panel.appendChild(panelBody);

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
    } else if (action === "show-comments") {
      togglePanel();
    } else if (action === "hide") {
      hideOverlay();
    }
  }

  // ---------- FAB dragging ------------------------------------------------

  var FAB_MARGIN = 16; // minimum distance from viewport edge
  var __fabCX = 0, __fabCY = 0; // logical center of FAB (avoids reading DOM)

  function initDrag() {
    var startX, startY, origX, origY, dragging = false, moved = false;

    fab.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      dragging = true; moved = false;
      startX = e.clientX; startY = e.clientY;
      origX = __fabCX;
      origY = __fabCY;
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
        saveFabPos(__fabCX, __fabCY);
      }
    });
  }

  function positionFab(cx, cy, animate) {
    __fabCX = cx;
    __fabCY = cy;
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
    positionFab(20 + FAB_MARGIN, window.innerHeight - 20 - FAB_MARGIN, false);
  }

  function clampFabToViewport() {
    var cx = __fabCX, cy = __fabCY;
    var clamped = false;
    var maxX = window.innerWidth - 20 - FAB_MARGIN;
    var maxY = window.innerHeight - 20 - FAB_MARGIN;
    var minXY = 20 + FAB_MARGIN;
    if (cx > maxX) { cx = maxX; clamped = true; }
    if (cx < minXY) { cx = minXY; clamped = true; }
    if (cy > maxY) { cy = maxY; clamped = true; }
    if (cy < minXY) { cy = minXY; clamped = true; }
    if (clamped) { positionFab(cx, cy, false); saveFabPos(cx, cy); }
  }

  // ---------- toolbar toggle ----------------------------------------------

  function toggleToolbar() {
    __veld_toolbarOpen = !__veld_toolbarOpen;
    toolbar.classList.toggle(PREFIX + "toolbar-open", __veld_toolbarOpen);
    if (!__veld_toolbarOpen) {
      setMode(null);
    }
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
        // After the browser's getDisplayMedia dialog, focus often shifts away
        // from the page. Re-focus and hint the user to start drawing.
        window.focus();
        toast("Draw a rectangle to capture a screenshot");
      }).catch(function () {
        toast("Screen capture denied", true);
        // Revert mode since we can't capture.
        __veld_activeMode = null;
        toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
      });
    }
  }

  // ---------- screenshot capture -------------------------------------------

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
      showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
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
        showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
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
        showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
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
      showScreenshotThreadEditor(pngBlob, screenshotId, viewX, viewY, viewW, viewH);
    }).catch(function (err) {
      toast("Screenshot upload failed: " + err.message, true);
      // Still show the editor, just without the stored screenshot.
      showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
    });
  }

  function showScreenshotThreadEditor(pngBlob, screenshotId, viewX, viewY, viewW, viewH) {
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
    pop._veldCleanup = function () {
      if (previewUrl) { URL.revokeObjectURL(previewUrl); previewUrl = null; }
    };

    var header = mkEl("div", "popover-header", "Screenshot \u2014 " + window.location.pathname);
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
    var sendBtn = mkEl("button", "btn btn-primary", "Send" + SUBMIT_HINT);
    sendBtn.addEventListener("click", function () {
      var text = ta.value.trim();
      if (!text) { ta.focus(); return; }
      if (sendBtn.disabled) return;
      sendBtn.disabled = true;
      var scope = {
        type: "page",
        page_url: window.location.pathname
      };
      var payload = {
        scope: scope,
        message: text,
        component_trace: null,
        screenshot: screenshotId || null,
        viewport_width: window.innerWidth,
        viewport_height: window.innerHeight
      };
      api("POST", "/threads", payload).then(function (thread) {
        __veld_threads.push(thread);
        closeActivePopover();
        addPin(thread);
        updateBadge();
        if (__veld_panelOpen) renderPanel();
        toast("Thread created");
      }).catch(function (err) {
        sendBtn.disabled = false;
        toast("Failed to create thread: " + err.message, true);
      });
    });
    actions.appendChild(cancelBtn);
    actions.appendChild(sendBtn);
    submitOnModEnter(ta, sendBtn);
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

  // ---------- backdrop events (select-element + screenshot drag) ----------

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

  // ---------- create popover (element-scoped thread) ----------------------

  function showCreatePopover(rect, selector, tagInfo, targetEl, trace) {
    closeActivePopover();

    // Lock the element highlight while the popover is open
    __veld_lockedEl = targetEl;

    var popover = mkEl("div", "popover");

    // Header
    if (selector) {
      var selectorEl = mkEl("div", "popover-selector", selector);
      popover.appendChild(selectorEl);
    }
    if (trace) {
      var traceEl = mkEl("div", "popover-trace", formatTrace(trace));
      popover.appendChild(traceEl);
    }

    // Popover body (provides padding)
    var popoverBody = mkEl("div", "popover-body");

    // Textarea
    var textarea = document.createElement("textarea");
    textarea.className = PREFIX + "textarea";
    textarea.placeholder = "Leave feedback...";
    textarea.rows = 3;
    popoverBody.appendChild(textarea);

    // Actions
    var actions = mkEl("div", "popover-actions");
    var cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Cancel");
    cancelBtn.addEventListener("click", closeActivePopover);
    actions.appendChild(cancelBtn);

    var sendBtn = mkEl("button", "btn btn-primary btn-sm", "Send" + SUBMIT_HINT);
    sendBtn.addEventListener("click", function () {
      var text = textarea.value.trim();
      if (!text) return;
      if (sendBtn.disabled) return;
      sendBtn.disabled = true;
      var scope = selector ? {
        type: "element",
        page_url: window.location.pathname,
        selector: selector,
        position: rect ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null
      } : {
        type: "page",
        page_url: window.location.pathname
      };
      var body = {
        scope: scope,
        message: text,
        component_trace: trace || null,
        viewport_width: window.innerWidth,
        viewport_height: window.innerHeight
      };
      api("POST", "/threads", body).then(function (thread) {
        __veld_threads.push(thread);
        closeActivePopover();
        addPin(thread);
        updateBadge();
        if (__veld_panelOpen) renderPanel();
        toast("Thread created");
      }).catch(function () {
        sendBtn.disabled = false;
        toast("Failed to create thread", true);
      });
    });
    actions.appendChild(sendBtn);
    submitOnModEnter(textarea, sendBtn);
    popoverBody.appendChild(actions);
    popover.appendChild(popoverBody);

    document.body.appendChild(popover);
    __veld_activePopover = popover;
    positionPopover(popover, rect);
    textarea.focus();
  }

  // ---------- page comment (page-scoped thread) ---------------------------

  function togglePageComment() {
    if (__veld_activePopover) { closeActivePopover(); return; }
    // Create a page-scoped thread (no element)
    showCreatePopover(
      { x: window.innerWidth / 2 - 180 + window.scrollX, y: 120 + window.scrollY, width: 0, height: 0 },
      null, null, null, null
    );
    toolBtnPageComment.classList.add(PREFIX + "tool-active");
  }

  // ---------- thread helpers ----------------------------------------------

  function findThread(id) {
    for (var i = 0; i < __veld_threads.length; i++) {
      if (__veld_threads[i].id === id) return __veld_threads[i];
    }
    return null;
  }

  function getThreadPageUrl(thread) {
    if (thread.scope.type === "element") return thread.scope.page_url;
    if (thread.scope.type === "page") return thread.scope.page_url;
    return null;
  }

  function getThreadPosition(thread) {
    if (thread.scope.type === "element" && thread.scope.position) return thread.scope.position;
    return null;
  }

  function isCurrentPage(url) {
    var path = url.split("?")[0];
    return path === window.location.pathname;
  }

  function hasUnread(thread) {
    if (!thread.messages) return false;
    var lastSeen = __veld_lastSeenAt[thread.id] || 0;
    // Check if the last message is from agent and thread has been updated since last seen
    var lastMsg = thread.messages[thread.messages.length - 1];
    return lastMsg && lastMsg.author === "agent" && (!lastSeen || new Date(thread.updated_at).getTime() > lastSeen);
  }

  function timeAgo(dateStr) {
    var d = new Date(dateStr);
    var s = Math.floor((Date.now() - d.getTime()) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return Math.floor(s / 60) + "m ago";
    if (s < 86400) return Math.floor(s / 3600) + "h ago";
    return Math.floor(s / 86400) + "d ago";
  }

  // ---------- badge -------------------------------------------------------

  function updateBadge() {
    var count = __veld_threads.filter(function (t) {
      return t.status === "open" && hasUnread(t);
    }).length;
    fabBadge.textContent = count || "";
    fabBadge.className = PREFIX + "badge" + (count ? "" : " " + PREFIX + "badge-hidden");
  }

  // ---------- panel -------------------------------------------------------

  function togglePanel() {
    __veld_panelOpen = !__veld_panelOpen;
    // Opening the panel always shows the list view (not a stale detail)
    if (__veld_panelOpen) __veld_expandedThreadId = null;
    panel.classList.toggle(PREFIX + "panel-open", __veld_panelOpen);
    if (__veld_panelOpen) renderPanel();
  }

  function showThreadDetail(threadId) {
    __veld_expandedThreadId = threadId;
    renderPanel();
  }

  function showThreadList() {
    __veld_expandedThreadId = null;
    renderPanel();
  }

  function openThreadInPanel(threadId) {
    __veld_panelOpen = true;
    __veld_panelTab = "active";
    __veld_expandedThreadId = threadId;
    panel.classList.add(PREFIX + "panel-open");
    renderPanel();
  }

  function updateSegmentedControl() {
    if (segBtnActive && segBtnResolved) {
      var activeCount = __veld_threads.filter(function (t) { return t.status === "open"; }).length;
      var resolvedCount = __veld_threads.filter(function (t) { return t.status === "resolved"; }).length;
      segBtnActive.textContent = "Active" + (activeCount ? " (" + activeCount + ")" : "");
      segBtnResolved.textContent = "Resolved" + (resolvedCount ? " (" + resolvedCount + ")" : "");
      segBtnActive.className = PREFIX + "segmented-btn" + (__veld_panelTab === "active" ? " " + PREFIX + "segmented-btn-active" : "");
      segBtnResolved.className = PREFIX + "segmented-btn" + (__veld_panelTab === "resolved" ? " " + PREFIX + "segmented-btn-active" : "");
    }
  }

  function renderPanel() {
    panelBody.innerHTML = "";

    // Two-layer panel: if a thread is expanded, show detail view
    if (__veld_expandedThreadId) {
      var thread = findThread(__veld_expandedThreadId);
      if (thread) {
        // Detail view: back button, hide segmented control + mark-read
        panelBackBtn.style.display = "";
        segControl.style.display = "none";
        if (markReadBtn) markReadBtn.style.display = "none";
        panelHeadTitle.textContent = "Thread";
        renderThreadDetail(thread);
        return;
      }
      __veld_expandedThreadId = null;
    }

    // List view: hide back button, show segmented control
    panelBackBtn.style.display = "none";
    segControl.style.display = "";
    panelHeadTitle.textContent = "Threads";
    updateSegmentedControl();
    updateMarkReadBtn();

    if (__veld_panelTab === "active") {
      renderActiveThreads();
    } else {
      renderResolvedThreads();
    }
  }

  var COPY_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>';

  function makeCopyRow(label, value, cls) {
    var row = mkEl("div", cls);
    row.appendChild(document.createTextNode(label + value));
    var icon = mkEl("span", "panel-detail-copy-icon");
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

  function renderThreadDetail(thread) {
    var header = mkEl("div", "panel-detail-header");

    // 1. Thread ID (tiny, first)
    header.appendChild(makeCopyRow("ID: ", thread.id.substring(0, 20) + "\u2026", "panel-detail-id"));

    // 2. Title — scope-aware with page context
    var pageUrl = getThreadPageUrl(thread);
    var titleText;
    if (thread.scope.type === "global") {
      titleText = "Global";
    } else if (thread.scope.type === "page") {
      titleText = "Page " + pageUrl;
      if (pageUrl === "/") titleText += " (home)";
    } else {
      titleText = "Page " + (pageUrl || "?");
      if (pageUrl === "/") titleText += " (home)";
    }
    var titleEl = mkEl("div", "panel-detail-title", titleText);
    header.appendChild(titleEl);

    // "Go to comment" link — scrolls to the pin/element on the page.
    // Works cross-page too (navigates first, then scrolls).
    // Only show for element-scoped threads (which have pins), or for
    // any thread on a different page (navigates there).
    var onDifferentPage = pageUrl && pageUrl !== window.location.pathname;
    var hasScrollTarget = thread.scope.type === "element";
    if (hasScrollTarget || onDifferentPage) {
      var goLabel = onDifferentPage ? "Go to page \u2192" : "Go to comment \u2192";
      var goLink = mkEl("a", "panel-detail-page-link", goLabel);
      goLink.href = pageUrl || "#";
      goLink.addEventListener("click", function (e) {
        e.preventDefault();
        scrollToThread(thread.id);
      });
      header.appendChild(goLink);
    }

    // 3. Component trace or CSS selector (small, copyable)
    if (thread.component_trace && thread.component_trace.length) {
      var traceText = thread.component_trace.join(" > ");
      header.appendChild(makeCopyRow("", traceText, "panel-detail-trace"));
    }
    if (thread.scope.type === "element" && thread.scope.selector) {
      header.appendChild(makeCopyRow("", thread.scope.selector, "panel-detail-selector"));
    }

    panelBody.appendChild(header);

    if (thread.status === "resolved") {
      // Resolved: show messages read-only + reopen button
      var msgList = mkEl("div", "thread-messages-list");
      thread.messages.forEach(function (msg) {
        var msgEl = mkEl("div", "message message-" + msg.author);
        var icon = mkEl("span", "message-author-icon");
        icon.innerHTML = msg.author === "agent" ? ICONS.robot : ICONS.chat;
        msgEl.appendChild(icon);
        var body = mkEl("div", "message-body");
        body.appendChild(mkEl("div", "message-text", msg.body));
        var authorLabel = msg.author === "agent" ? "Agent" : "You";
        body.appendChild(mkEl("div", "message-meta", authorLabel + " \u00B7 " + timeAgo(msg.created_at)));
        msgEl.appendChild(body);
        msgList.appendChild(msgEl);
      });
      panelBody.appendChild(msgList);

      var reopenRow = mkEl("div", "thread-input-actions");
      var reopenBtn = mkEl("button", "btn btn-primary btn-sm", "Reopen Thread");
      reopenBtn.addEventListener("click", function () {
        api("POST", "/threads/" + thread.id + "/reopen").then(function () {
          thread.status = "open";
          showThreadList();
          renderAllPins();
          toast("Thread reopened");
        });
      });
      reopenRow.appendChild(reopenBtn);
      panelBody.appendChild(reopenRow);
    } else {
      // Active: show messages + reply input + resolve
      panelBody.appendChild(renderThreadMessages(thread));
    }
  }

  function renderActiveThreads() {
    var active = __veld_threads.filter(function (t) { return t.status === "open"; });
    if (!active.length) {
      panelBody.appendChild(mkEl("div", "panel-empty", "No active threads."));
      return;
    }

    // Group by page
    var global = [];
    var byPage = {};
    var pageOrder = [];
    active.forEach(function (t) {
      var url = getThreadPageUrl(t);
      if (!url) { global.push(t); return; }
      var path = url.split("?")[0];
      if (!byPage[path]) { byPage[path] = []; pageOrder.push(path); }
      byPage[path].push(t);
    });

    // Current page first
    pageOrder.sort(function (a, b) {
      if (a === window.location.pathname) return -1;
      if (b === window.location.pathname) return 1;
      return a.localeCompare(b);
    });

    if (global.length) renderThreadGroup("Global", global);
    pageOrder.forEach(function (p) {
      var label = "Page " + p;
      if (p === "/") label += " (home)";
      renderThreadGroup(label, byPage[p]);
    });
  }

  function renderResolvedThreads() {
    var resolved = __veld_threads.filter(function (t) { return t.status === "resolved"; });
    if (!resolved.length) {
      panelBody.appendChild(mkEl("div", "panel-empty", "No resolved threads."));
      return;
    }
    resolved.sort(function (a, b) {
      return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
    });
    resolved.forEach(function (t) { panelBody.appendChild(makeThreadCard(t, true)); });
  }

  function renderThreadGroup(label, threads) {
    // Sort by latest activity (newest first)
    threads.sort(function (a, b) {
      return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
    });
    var section = mkEl("div", "panel-section");
    var heading = mkEl("div", "panel-section-heading", label);
    section.appendChild(heading);
    threads.forEach(function (t) { section.appendChild(makeThreadCard(t, false)); });
    panelBody.appendChild(section);
  }

  // ---------- thread card -------------------------------------------------

  function makeThreadCard(thread, isResolved) {
    var card = mkEl("div", "thread-card" + (isResolved ? " thread-card-resolved" : ""));
    if (hasUnread(thread) && !isResolved) card.classList.add(PREFIX + "thread-card-unread");
    card.dataset.threadId = thread.id;

    // Row 1: preview text + meta (compact)
    var row1 = mkEl("div", "thread-card-row");
    var preview = (thread.messages && thread.messages[0]) ? thread.messages[0].body : "";
    if (preview.length > 50) preview = preview.substring(0, 50) + "\u2026";
    row1.appendChild(mkEl("span", "thread-card-preview", preview));
    var msgCount = thread.messages ? thread.messages.length : 0;
    var metaText = msgCount > 1 ? msgCount + " replies" : "";
    if (metaText) metaText += " \u00B7 ";
    metaText += timeAgo(thread.updated_at);
    row1.appendChild(mkEl("span", "thread-card-meta", metaText));
    card.appendChild(row1);

    // Row 2: element selector (only for element-scoped, keeps it contextual)
    if (thread.scope && thread.scope.type === "element" && thread.scope.selector) {
      card.appendChild(mkEl("div", "thread-card-selector", thread.scope.selector));
    }

    // Click card to open detail view (both active and resolved)
    card.addEventListener("click", function () {
      showThreadDetail(thread.id);
    });

    return card;
  }

  // ---------- thread messages view ----------------------------------------

  function renderThreadMessages(thread) {
    var container = mkEl("div", "thread-messages");

    // Messages
    var msgList = mkEl("div", "thread-messages-list");
    thread.messages.forEach(function (msg) {
      var msgEl = mkEl("div", "message message-" + msg.author);
      var icon = mkEl("span", "message-author-icon");
      icon.innerHTML = msg.author === "agent" ? ICONS.robot : ICONS.chat;
      msgEl.appendChild(icon);
      var body = mkEl("div", "message-body");
      body.appendChild(mkEl("div", "message-text", msg.body));
      var authorLabel = msg.author === "agent" ? "Agent" : "You";
      body.appendChild(mkEl("div", "message-meta", authorLabel + " \u00B7 " + timeAgo(msg.created_at)));
      msgEl.appendChild(body);
      msgList.appendChild(msgEl);
    });
    container.appendChild(msgList);

    // Mark as seen
    markThreadSeen(thread.id);

    // Reply input
    var input = mkEl("div", "thread-input");
    var textarea = document.createElement("textarea");
    textarea.className = PREFIX + "textarea";
    textarea.placeholder = "Reply...";
    textarea.rows = 2;
    input.appendChild(textarea);

    var inputActions = mkEl("div", "thread-input-actions");
    var resolveBtn = mkEl("button", "btn btn-secondary btn-sm", "Resolve \u2713");
    resolveBtn.addEventListener("click", function () {
      var text = textarea.value.trim();
      var doResolve = function () {
        api("POST", "/threads/" + thread.id + "/resolve").then(function () {
          thread.status = "resolved";
          closeActivePopover();
          showThreadList();
          renderAllPins();
          toast("Thread resolved");
        });
      };
      // If there's text, send it as a comment first, then resolve
      if (text) {
        api("POST", "/threads/" + thread.id + "/messages", { body: text }).then(function (msg) {
          thread.messages.push(msg);
          doResolve();
        });
      } else {
        doResolve();
      }
    });
    inputActions.appendChild(resolveBtn);

    var sendBtn = mkEl("button", "btn btn-primary btn-sm", "Send" + SUBMIT_HINT);
    sendBtn.addEventListener("click", function () {
      var text = textarea.value.trim();
      if (!text) return;
      if (sendBtn.disabled) return;
      sendBtn.disabled = true;
      api("POST", "/threads/" + thread.id + "/messages", { body: text }).then(function (msg) {
        thread.messages.push(msg);
        thread.updated_at = new Date().toISOString();
        textarea.value = "";
        sendBtn.disabled = false;
        if (__veld_panelOpen) renderPanel();
        renderAllPins();
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

  // ---------- mark seen ---------------------------------------------------

  function markThreadSeen(threadId) {
    __veld_lastSeenAt[threadId] = Date.now();
    var thread = findThread(threadId);
    if (thread) {
      api("PUT", "/threads/" + threadId + "/seen", { seq: __veld_lastEventSeq }).catch(function () {});
      addPin(thread);
      updateBadge();
      updateMarkReadBtn();
    }
  }

  function markAllRead() {
    __veld_threads.forEach(function (t) {
      if (t.status === "open" && hasUnread(t)) {
        __veld_lastSeenAt[t.id] = Date.now();
        api("PUT", "/threads/" + t.id + "/seen", { seq: __veld_lastEventSeq }).catch(function () {});
      }
    });
    renderAllPins();
    updateBadge();
    updateMarkReadBtn();
    if (__veld_panelOpen) renderPanel();
    toast("All marked as read");
  }

  function updateMarkReadBtn() {
    if (!markReadBtn) return;
    var hasAny = __veld_threads.some(function (t) {
      return t.status === "open" && hasUnread(t);
    });
    markReadBtn.style.display = hasAny ? "" : "none";
  }

  // ---------- pins --------------------------------------------------------

  function addPin(thread) {
    if (thread.status === "resolved") return;
    // Only pin element-scoped threads on the current page
    var pageUrl = getThreadPageUrl(thread);
    if (!pageUrl || !isCurrentPage(pageUrl)) return;
    var pos = getThreadPosition(thread);
    if (!pos) return;

    removePin(thread.id);

    var pin = mkEl("div", "pin");
    pin.id = PREFIX + "pin-" + thread.id;
    pin.dataset.threadId = thread.id;

    // Chat icon
    var icon = mkEl("span", "pin-icon");
    icon.innerHTML = ICONS.chat;
    pin.appendChild(icon);

    // Message count (if > 1)
    var msgCount = thread.messages ? thread.messages.length : 1;
    if (msgCount > 1) {
      var count = mkEl("span", "pin-count", String(msgCount));
      pin.appendChild(count);
    }

    // Unread dot
    if (hasUnread(thread)) {
      var dot = mkEl("span", "pin-unread-dot");
      pin.appendChild(dot);
    }

    pin.style.position = "absolute";
    pin.style.top = (pos.y - 12) + "px";
    pin.style.left = (pos.x + pos.width - 12) + "px";
    pin.style.zIndex = "calc(var(--vf-z) - 1)";

    pin.addEventListener("click", function (e) {
      e.stopPropagation();
      openThreadInPanel(thread.id);
    });

    document.body.appendChild(pin);
    __veld_pins[thread.id] = pin;
  }

  function removePin(threadId) {
    if (__veld_pins[threadId]) {
      __veld_pins[threadId].remove();
      delete __veld_pins[threadId];
    }
  }

  function renderAllPins() {
    Object.keys(__veld_pins).forEach(removePin);
    __veld_threads.forEach(function (t) {
      if (t.status === "open") addPin(t);
    });
  }

  function repositionPins() {
    __veld_threads.forEach(function (t) {
      var pin = __veld_pins[t.id];
      if (!pin) return;
      if (!t.scope || t.scope.type !== "element" || !t.scope.selector) return;
      try {
        var el = document.querySelector(t.scope.selector);
        if (el) {
          var r = docRect(el);
          t.scope.position = { x: r.x, y: r.y, width: r.width, height: r.height };
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

  // ---------- go-to-comment ------------------------------------------------

  var SCROLL_TO_KEY = "veld-feedback-scroll-to-thread";

  /**
   * Scroll the page so that the pin (bubble) for the given thread is visible,
   * then briefly highlight it.  If the thread lives on a different page,
   * navigate there first — the scroll will happen after navigation via
   * sessionStorage.
   */
  function scrollToThread(threadId) {
    var thread = findThread(threadId);
    if (!thread) return;

    var pageUrl = getThreadPageUrl(thread);

    // Different page — navigate first, scroll after load.
    if (pageUrl && !isCurrentPage(pageUrl)) {
      try { sessionStorage.setItem(SCROLL_TO_KEY, threadId); } catch (_) {}
      window.location.href = pageUrl;
      return;
    }

    // Same page — try to scroll to the target element first (more useful
    // than scrolling to the pin itself), fall back to the pin.
    var target = null;
    if (thread.scope && thread.scope.type === "element" && thread.scope.selector) {
      try { target = document.querySelector(thread.scope.selector); } catch (_) {}
    }
    if (!target) target = __veld_pins[threadId] || document.getElementById(PREFIX + "pin-" + threadId);
    if (!target) return;

    target.scrollIntoView({ behavior: "smooth", block: "center" });

    // Highlight the pin after scroll settles.
    var pin = __veld_pins[threadId];
    if (pin) {
      setTimeout(function () {
        // Remove + reflow to restart animation if already highlighted.
        pin.classList.remove(PREFIX + "pin-highlight");
        void pin.offsetWidth;
        pin.classList.add(PREFIX + "pin-highlight");
        setTimeout(function () { pin.classList.remove(PREFIX + "pin-highlight"); }, 1500);
      }, 400);
    }
  }

  /** Check sessionStorage for a pending scroll-to-thread request. */
  function checkPendingScroll() {
    try {
      var id = sessionStorage.getItem(SCROLL_TO_KEY);
      if (id) {
        sessionStorage.removeItem(SCROLL_TO_KEY);
        // Give the page a moment to render / hydrate before scrolling.
        setTimeout(function () { scrollToThread(id); }, 300);
      }
    } catch (_) {}
  }

  // ---------- polling loops -----------------------------------------------

  function pollEvents() {
    api("GET", "/events?after=" + __veld_lastEventSeq).then(function (events) {
      if (!events || !events.length) return;
      events.forEach(function (event) {
        handleEvent(event);
        if (event.seq > __veld_lastEventSeq) __veld_lastEventSeq = event.seq;
      });
    }).catch(function () {});
  }

  function pollListenStatus() {
    api("GET", "/session").then(function (data) {
      var wasListening = __veld_agentListening;
      __veld_agentListening = data && data.listening;
      if (__veld_agentListening !== wasListening) updateListeningModule();
    }).catch(function () {});
  }

  // ---------- event handling ----------------------------------------------

  function handleEvent(event) {
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
        __veld_agentListening = true;
        updateListeningModule();
        break;
      case "agent_stopped":
        __veld_agentListening = false;
        updateListeningModule();
        toast("Agent stopped listening");
        break;
      case "session_ended":
        __veld_agentListening = false;
        updateListeningModule();
        break;
      case "thread_created":
        // Could be from another tab — merge if not already known
        if (event.thread && !findThread(event.thread.id)) {
          __veld_threads.push(event.thread);
          addPin(event.thread);
          updateBadge();
          if (__veld_panelOpen) renderPanel();
        }
        break;
      case "human_message":
        // Could be from another tab — update thread if known
        if (event.thread_id && event.message) {
          var hmThread = findThread(event.thread_id);
          if (hmThread) {
            var exists = hmThread.messages.some(function (m) { return m.id === event.message.id; });
            if (!exists) {
              hmThread.messages.push(event.message);
              hmThread.updated_at = event.message.created_at || new Date().toISOString();
              if (__veld_panelOpen) renderPanel();
            }
          }
        }
        break;
    }
  }

  function handleAgentMessage(event) {
    var thread = findThread(event.thread_id);
    if (!thread) {
      // Thread not loaded yet — fetch it individually
      api("GET", "/threads/" + event.thread_id).then(function (t) {
        if (t) {
          __veld_threads.push(t);
          addPin(t);
          updateBadge();
          if (__veld_panelOpen) renderPanel();
          showAgentReplyToast(t.id, event.message.body);
        }
      }).catch(function () {});
      return;
    }

    // Add the message if not already present
    if (event.message) {
      var exists = false;
      for (var i = 0; i < thread.messages.length; i++) {
        if (thread.messages[i].id === event.message.id) { exists = true; break; }
      }
      if (!exists) {
        thread.messages.push(event.message);
        thread.updated_at = event.message.created_at || new Date().toISOString();
      }
    }

    // Re-render pin (show red dot for unread)
    addPin(thread);
    updateBadge();

    // If panel is open, re-render panel
    if (__veld_panelOpen) renderPanel();

    // Show agent reply toast
    var preview = event.message ? event.message.body : "New reply";
    showAgentReplyToast(event.thread_id, preview);

    // Send browser notification if tab not focused
    if (!document.hasFocus()) {
      sendBrowserNotification("Agent replied", preview, event.thread_id);
    }
  }

  function handleAgentThreadCreated(event) {
    if (event.thread) {
      var existing = findThread(event.thread.id);
      if (!existing) {
        __veld_threads.push(event.thread);
        addPin(event.thread);
        updateBadge();
        if (__veld_panelOpen) renderPanel();

        var preview = event.thread.messages && event.thread.messages[0] ? event.thread.messages[0].body : "New thread";
        showAgentReplyToast(event.thread.id, preview);

        if (!document.hasFocus()) {
          sendBrowserNotification("Agent started a thread", preview, event.thread.id);
        }
      }
    } else {
      loadThreads();
    }
  }

  function handleThreadResolved(event) {
    var thread = findThread(event.thread_id);
    if (thread) {
      thread.status = "resolved";
      removePin(thread.id);
      updateBadge();
      if (__veld_panelOpen) renderPanel();
    }
  }

  function handleThreadReopened(event) {
    var thread = findThread(event.thread_id);
    if (thread) {
      thread.status = "open";
      addPin(thread);
      updateBadge();
      if (__veld_panelOpen) renderPanel();
    }
  }

  // ---------- listening module --------------------------------------------

  function updateListeningModule() {
    if (listeningModule) {
      listeningModule.style.display = __veld_agentListening ? "flex" : "none";
    }
    if (fab) {
      fab.classList.toggle(PREFIX + "fab-pulse", __veld_agentListening);
    }
  }

  function sendAllGood() {
    api("POST", "/session/end").then(function () {
      toast("All Good signal sent!");
      __veld_agentListening = false;
      updateListeningModule();
    }).catch(function (err) {
      toast("Failed: " + err.message, true);
    });
  }

  // ---------- agent reply toast -------------------------------------------

  function showAgentReplyToast(threadId, preview) {
    var t = mkEl("div", "agent-toast");
    t.appendChild(mkEl("div", "agent-toast-header", "Agent replied"));
    var body = mkEl("div", "agent-toast-body");
    body.textContent = preview.length > 60 ? preview.substring(0, 60) + "..." : preview;
    t.appendChild(body);
    var link = mkEl("button", "agent-toast-link", "Go to thread \u2192");
    link.addEventListener("click", function () {
      t.remove();
      openThreadInPanel(threadId);
      scrollToThread(threadId);
    });
    t.appendChild(link);
    document.body.appendChild(t);
    requestAnimationFrame(function () { t.classList.add(PREFIX + "agent-toast-show"); });
    setTimeout(function () {
      t.classList.remove(PREFIX + "agent-toast-show");
      setTimeout(function () { t.remove(); }, 300);
    }, 8000);
  }

  // ---------- browser notifications ---------------------------------------

  function sendBrowserNotification(title, body, threadId) {
    if (!("Notification" in window) || Notification.permission !== "granted") return;
    var n = new Notification(title, {
      body: body,
      icon: "/__veld__/feedback/logo.svg",
      tag: "veld-thread-" + threadId
    });
    n.addEventListener("click", function () {
      window.focus();
      openThreadInPanel(threadId);
      scrollToThread(threadId);
      n.close();
    });
  }

  // ---------- load threads (initial hydration) ----------------------------

  function loadThreads() {
    api("GET", "/threads").then(function (threads) {
      __veld_threads = threads || [];
      renderAllPins();
      updateBadge();
      checkPendingScroll();
      if (__veld_panelOpen) renderPanel();
    }).catch(function () {});
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

    // Mod+Shift+C: toggle panel
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

  // ---------- SPA navigation detection ------------------------------------

  var __veld_lastPathname = window.location.pathname;

  function onNavigate() {
    var newPath = window.location.pathname;
    if (newPath !== __veld_lastPathname) {
      __veld_lastPathname = newPath;
      renderAllPins();
      if (__veld_panelOpen) renderPanel();
      checkPendingScroll();
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
    restoreFabPos();
    clampFabToViewport();

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

    loadThreads();
    pollEvents();
    pollListenStatus();
    setInterval(pollEvents, 3000);
    setInterval(pollListenStatus, 5000);

    // Pending scroll is checked inside loadThreads() callback, after threads
    // are hydrated — not here, where the async fetch hasn't completed yet.

    // Request notification permission
    if ("Notification" in window && Notification.permission === "default") {
      Notification.requestPermission();
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
