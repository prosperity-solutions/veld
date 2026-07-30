// Validation for everything the page hands the shell.
//
// Its own module for one reason: this is the **trust boundary**, and it was
// previously inline in `browserViews.js` where nothing was exported and the
// desktop package had no test runner — so the copy that actually guards the main
// process was the one with no tests, while its renderer-side counterpart
// (`normalizeBrowserUrl` in `crates/veld-daemon/ui/src/panes/model.ts`) was
// covered. The renderer's checks are defence-in-depth; these are the real gate,
// because a renderer is not a trust boundary.
//
// Pure and dependency-free, so `node --test src/validate.test.js` runs it without
// an Electron binary.

/**
 * View ids come from the page. Kept to the same charset the daemon accepts for a
 * PTY session id, since a browser tab id and a terminal tab id are minted by the
 * same generator (`newTabId`).
 */
const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

/**
 * Session slot names, which become the tail of an Electron partition string.
 * Lowercase, no dots, no slashes, bounded — so a name can never traverse or
 * collide with another namespace's partition.
 */
const PROFILE_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;

/**
 * Only `http(s)`. A browser pane is a preview of a dev server, and every other
 * scheme a URL parser accepts turns one into something else — `file:` into a
 * local-file reader, `javascript:` into script in the pane's own origin, `blob:`
 * and `data:` into content with no origin to attribute.
 *
 * Returns the normalised URL, or `null` to reject. Never throws.
 */
function safeUrl(raw) {
  if (typeof raw !== "string" || raw === "") return null;
  let u;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  return u.toString();
}

/** Whether the page may address a view by this id. */
function isViewId(id) {
  return typeof id === "string" && ID_RE.test(id);
}

/** Whether the page may name this session slot. */
function isProfileName(name) {
  return typeof name === "string" && PROFILE_RE.test(name);
}

/** `persist:` so a slot's cookies survive a restart — the point of naming one.
 *  Namespaced so it can never collide with the app's own session. */
function partitionFor(profile) {
  return `persist:veld-browser-${profile}`;
}

module.exports = { ID_RE, PROFILE_RE, safeUrl, isViewId, isProfileName, partitionFor };
