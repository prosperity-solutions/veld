import { S } from "./state";
import { hasUnread } from "./helpers";
import { PREFIX } from "./constants";

/** Update the FAB badge to show the count of unread open threads. */
export function updateBadge(): void {
  const count = S.threads.filter((t) => {
    return t.status === "open" && hasUnread(t, S.lastSeenAt);
  }).length;
  S.fabBadge.textContent = count ? String(count) : "";
  S.fabBadge.className =
    PREFIX + "badge" + (count ? "" : " " + PREFIX + "badge-hidden");
}
