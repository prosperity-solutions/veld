import { refs } from "./refs";
import { store } from "./store";
import { hasUnread } from "./helpers";
import { PREFIX } from "./constants";

/** Update the FAB badge to show the count of unread open threads. */
export function updateBadge(): void {
  const count = store.threads.filter((t) => {
    return t.status === "open" && hasUnread(t, store.lastSeenAt);
  }).length;
  refs.fabBadge.textContent = count ? String(count) : "";
  refs.fabBadge.className =
    PREFIX + "badge" + (count ? "" : " " + PREFIX + "badge-hidden");
}
