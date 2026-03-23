import { refs } from "./refs";
import { getState } from "./store";
import { hasUnread } from "./helpers";
import { PREFIX } from "./constants";

/** Update badges: FAB badge when toolbar is closed, threads button badge when open. */
export function updateBadge(): void {
  const count = getState().threads.filter((t) => {
    return t.status === "open" && hasUnread(t, getState().lastSeenAt);
  }).length;
  const hasCount = count > 0;
  const toolbarOpen = getState().toolbarOpen;

  // FAB badge: show when toolbar is closed
  refs.fabBadge.textContent = hasCount ? String(count) : "";
  refs.fabBadge.className =
    PREFIX + "badge" + (hasCount && !toolbarOpen ? "" : " " + PREFIX + "badge-hidden");

  // Threads button badge: show when toolbar is open
  let btnBadge = refs.toolBtnComments.querySelector("." + PREFIX + "tool-badge") as HTMLElement | null;
  if (hasCount && toolbarOpen) {
    if (!btnBadge) {
      btnBadge = document.createElement("span");
      btnBadge.className = PREFIX + "tool-badge";
      refs.toolBtnComments.appendChild(btnBadge);
    }
    btnBadge.textContent = String(count);
    btnBadge.style.display = "";
  } else if (btnBadge) {
    btnBadge.style.display = "none";
  }
}
