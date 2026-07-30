import { useEffect, useRef, useState } from "react";

/**
 * Copy-to-clipboard with a per-target "Copied" flash.
 *
 * Tagged rather than boolean because a row of copy buttons shares one hook: the
 * tag says *which* button just fired, so the other labels stay put. The timer is
 * cleared on unmount and re-armed per copy — a pane can be closed inside the
 * 1.5s window, and setting state after that is a React warning at best.
 */
export function useCopyFlash(): { flash: string | null; copy: (text: string, tag: string) => void } {
  const [flash, setFlash] = useState<string | null>(null);
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = (text: string, tag: string) => {
    void navigator.clipboard.writeText(text);
    setFlash(tag);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setFlash(null), 1500);
  };
  return { flash, copy };
}
