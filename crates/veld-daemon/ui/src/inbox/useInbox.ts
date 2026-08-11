import { useEffect, useReducer } from "react";

import { inbox } from "./inbox";

/**
 * Re-render when the inbox changes.
 *
 * Returns nothing: the caller reads what it needs through `inbox.counts(...)` /
 * `inbox.entries(...)` on each render. That is the pattern `panes/PaneArea.tsx` already
 * uses for the terminal and browser hosts (`subscribeTerminal` + a `useReducer` bump +
 * `terminalStatus(id)`), and it is what makes returning a fresh object per read safe —
 * there is no `getSnapshot` for React to compare, so no referential-stability rule to
 * break and no cached snapshot to invalidate.
 */
export function useInbox(): void {
  const [, bump] = useReducer((n: number) => n + 1, 0);
  useEffect(() => inbox.subscribe(bump), []);
}
