import type { LogEntry } from "./types";
import { stringify, captureStack } from "./formatter";

(function () {
  "use strict";
  try {
    // Dedup guard — bootstrap sets __veld_cl=1, full script upgrades to 2.
    if ((window as any).__veld_cl >= 2) return;
    (window as any).__veld_cl = 2;
    // Top-frame only — avoid double-collection in iframes.
    if (window !== window.top) return;

    const sc =
      document.currentScript ||
      document.querySelector('script[src*="client-log.js"]');
    const levelsAttr = sc && sc.getAttribute("data-veld-levels");
    const levels = levelsAttr ? levelsAttr.split(",") : [];
    const levelSet: Record<string, number> = {};
    for (let i = 0; i < levels.length; i++) levelSet[levels[i]] = 1;

    let buf: LogEntry[] = [];
    // Drain early logs captured by the bootstrap script.
    const early = (window as any).__veld_early_logs as LogEntry[] | undefined;
    if (early && early.length) {
      for (let j = 0; j < early.length; j++) buf.push(early[j]);
      (window as any).__veld_early_logs = null;
    }
    let timer: ReturnType<typeof setTimeout> | null = null;
    const endpoint = "/__veld__/api/client-logs";
    const MAX_BUF = 50;
    const FLUSH_MS = 1000;

    function flush(): void {
      if (!buf.length) return;
      const batch = buf;
      buf = [];
      try {
        const x = new XMLHttpRequest();
        x.open("POST", endpoint, true);
        x.setRequestHeader("Content-Type", "application/json");
        x.send(JSON.stringify({ entries: batch }));
      } catch {}
    }

    function schedule(): void {
      if (timer) return;
      timer = setTimeout(function () {
        timer = null;
        flush();
      }, FLUSH_MS);
    }

    function push(entry: LogEntry): void {
      buf.push(entry);
      if (buf.length >= MAX_BUF) {
        if (timer) {
          clearTimeout(timer);
          timer = null;
        }
        flush();
      } else schedule();
    }

    function now(): string {
      return new Date().toISOString();
    }

    const needsStack: Record<string, number> = { error: 1, warn: 1 };
    const scriptUrl = (sc as HTMLScriptElement | null)?.src || "client-log.js";

    // Monkey-patch console methods.
    const con = window.console;
    const methods = ["log", "warn", "error", "info", "debug"] as const;
    for (let m = 0; m < methods.length; m++) {
      (function (name: string) {
        if (!levelSet[name]) return;
        const orig = (con as any)[name] as (...args: unknown[]) => void;
        if (typeof orig !== "function") return;
        (con as any)[name] = function (this: Console) {
          orig.apply(con, arguments as any);
          try {
            const entry: LogEntry = {
              ts: now(),
              level: name,
              msg: stringify(arguments),
            };
            if (needsStack[name]) entry.stack = captureStack(scriptUrl);
            push(entry);
          } catch {}
        };
      })(methods[m]);
    }

    // Capture unhandled exceptions — always, regardless of level config.
    window.addEventListener("error", function (ev: ErrorEvent) {
      try {
        push({
          ts: now(),
          level: "exception",
          msg: ev.message || String(ev),
          stack:
            ev.error && ev.error.stack
              ? ev.error.stack
              : ev.filename
                ? ev.filename + ":" + ev.lineno + ":" + ev.colno
                : "",
        });
      } catch {}
    });

    // Capture unhandled promise rejections — always.
    window.addEventListener(
      "unhandledrejection",
      function (ev: PromiseRejectionEvent) {
        try {
          const reason = ev.reason;
          const msg =
            reason instanceof Error ? reason.message : String(reason || "");
          const stack =
            reason instanceof Error && reason.stack ? reason.stack : "";
          push({
            ts: now(),
            level: "exception",
            msg: "Unhandled Promise rejection: " + msg,
            stack,
          });
        } catch {}
      },
    );

    // Flush on page unload using sendBeacon (survives page navigation).
    window.addEventListener("beforeunload", function () {
      if (!buf.length) return;
      const data = JSON.stringify({ entries: buf });
      buf = [];
      if (navigator.sendBeacon) {
        navigator.sendBeacon(
          endpoint,
          new Blob([data], { type: "application/json" }),
        );
      } else {
        try {
          const x = new XMLHttpRequest();
          x.open("POST", endpoint, false);
          x.setRequestHeader("Content-Type", "application/json");
          x.send(data);
        } catch {}
      }
    });
  } catch {}
})();
