// Permission policy for embedded browser panes: the veld↔Electron id mapping,
// origin matching, and the precedence rules that turn a project config, a user's
// stored answers and veld's defaults into one verdict.
//
// Its own Electron-free module, for the same reason `windowState.js` and
// `validate.js` are: everything interesting here is table lookup and string
// parsing, and `node --test src/*.test.js` can reach it with no Electron binary.
// `browserViews.js` holds the half that needs a `Session`.
//
// **Why panes need a policy at all.** Until now every permission was refused
// outright, because a prompt raised by an embedded pane has no chrome to
// attribute it to — "example.com wants your camera" is a lie when the window says
// Veld. Blanket denial has its own cost, and one case had become a functional
// break: veld's own feedback overlay screenshots through
// `getDisplayMedia({ preferCurrentTab: true })`, so `veld feedback` could not take
// a screenshot inside a browser pane, which is the one place it should work best.
//
// So the answer is a policy with three sources, and a prompt that *can* name the
// pane, its session and the origin:
//
//   1. the user's own stored answer for (session, origin, permission)
//   2. the project's `ide.permissions` in `veld.json`, versioned with the repo
//   3. veld's defaults — what a browser would do without asking
//
// A config grant is deliberately powerful: a repo that can already run arbitrary
// commands through `veld start` is not meaningfully constrained by withholding
// camera access from its own dev server. What it is *not* is invisible — every
// config-sourced grant shows in the pane's per-site panel labelled as coming from
// veld.json, and a user answer always outranks it.

/**
 * Veld's permission ids.
 *
 * Kept in this order and asserted against `$defs.permissionId` in
 * `schema/v3/veld.schema.json` — by `permissions.test.js` here, by
 * `the_schema_enum_matches_the_parser` for `veld_core::ide::PERMISSION_IDS`, and
 * by `panes/permissions.test.ts` for the TypeScript union and its label map. The
 * schema is the one source; the other three are gated against it.
 *
 * Two prose copies are **not** gated — `docs/configuration.md` and
 * `skills/veld/reference/config.md` both list the ids for a reader. Adding an id
 * means editing those by hand.
 */
const VELD_PERMISSIONS = [
  "camera",
  "clipboard-read",
  "clipboard-write",
  "display-capture",
  "file-system",
  "fullscreen",
  "geolocation",
  "hid",
  "idle-detection",
  "keyboard-lock",
  "microphone",
  "midi",
  "notifications",
  "open-external",
  "pointer-lock",
  "protected-media",
  "serial",
  "speaker-selection",
  "storage-access",
  "usb",
  "window-management",
];

/**
 * Electron's permission names → veld's.
 *
 * Electron 43's request and check unions are not the same set, and this table
 * covers both — `hid`, `serial`, `usb` and `deprecated-sync-clipboard-read` only
 * ever appear in a *check*, while `display-capture`, `window-management`,
 * `speaker-selection` and `keyboardLock` only appear in a *request*.
 *
 * Three names collapse onto one id on purpose: sysex is not a separate switch in
 * any browser's UI, the deprecated synchronous clipboard read is the same
 * capability as the async one, and top-level storage access is storage access.
 * One name splits the other way — see `mediaIds`.
 */
const ELECTRON_TO_VELD = {
  "clipboard-read": "clipboard-read",
  "clipboard-sanitized-write": "clipboard-write",
  "deprecated-sync-clipboard-read": "clipboard-read",
  "display-capture": "display-capture",
  mediaKeySystem: "protected-media",
  fileSystem: "file-system",
  fullscreen: "fullscreen",
  geolocation: "geolocation",
  hid: "hid",
  "idle-detection": "idle-detection",
  keyboardLock: "keyboard-lock",
  midi: "midi",
  midiSysex: "midi",
  notifications: "notifications",
  openExternal: "open-external",
  pointerLock: "pointer-lock",
  serial: "serial",
  "speaker-selection": "speaker-selection",
  "storage-access": "storage-access",
  "top-level-storage-access": "storage-access",
  usb: "usb",
  "window-management": "window-management",
};

/**
 * The one Electron name answered without ever reaching the policy.
 *
 * `unknown` is Electron's escape hatch for a permission this version does not
 * model, and the only safe answer to a capability nobody can name is no. It is a
 * *denial*, which is why it can live here: it grants nothing, and there is
 * nothing for a per-site panel to show.
 *
 * Two allows used to sit beside it — sanitized clipboard **write** and encrypted
 * media — on the reasoning that no browser exposes either as a per-site switch.
 * That reasoning was wrong for this surface: an allow with no row in the panel is
 * one nobody can see and nobody can revoke, which is exactly the "hidden clever
 * default" a permission UI exists to abolish. They are ordinary permissions now
 * (`clipboard-write`, `protected-media`), they ask like everything else, and a
 * project that wants them silent says so in `veld.json`.
 */
const FIXED_VERDICTS = {
  unknown: "deny",
};

/**
 * What veld answers when neither the user nor the project has said anything.
 *
 * The rule for this table is "what would a browser do without asking": the two
 * capabilities below are granted on a user gesture by every browser and are
 * reversible with Escape, so prompting for them would make a pane *worse* than
 * the browser it embeds. Everything absent from this table defaults to `ask`.
 *
 * `keyboard-lock` was here and was removed: the justification is "reversible with
 * Escape", and capturing Escape is precisely what keyboard lock does — the one
 * entry the sentence was false for. The shell also has no
 * `enter-html-full-screen` handling for a pane whose bounds it owns, so there is
 * no exit affordance of veld's own to fall back on. One prompt in a rare case is
 * the cheaper side of that trade.
 *
 * `display-capture` is conditional rather than listed here — see `defaultVerdict`.
 */
const DEFAULT_ALLOW = new Set(["fullscreen", "pointer-lock"]);

/** Default ports, so an origin with no port compares equal to one with it. */
const DEFAULT_PORTS = { http: 80, https: 443 };

/**
 * Split a URL into a comparable origin, or `null` if it has none.
 *
 * `null` is the answer for `about:blank`, `data:` and a frame that has navigated
 * away — all of which reach the handlers, and none of which can be matched
 * against a rule. A caller that gets `null` must deny: there is nothing to
 * attribute a grant to.
 */
function parseOrigin(url) {
  if (typeof url !== "string" || url === "") return null;
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  const scheme = parsed.protocol.replace(/:$/, "").toLowerCase();
  if (scheme !== "http" && scheme !== "https") return null;
  if (!parsed.hostname) return null;
  // A single trailing dot is the fully-qualified spelling of the same name and
  // resolves identically, but it is a different string — so without this a page
  // escapes a user's Block by linking to the dotted form and getting a fresh
  // prompt, and every config rule quietly stops matching. Stripped here, once,
  // so the store key and the matcher agree.
  const host = parsed.hostname.toLowerCase().replace(/\.$/, "");
  if (host === "") return null;
  const port = parsed.port ? Number.parseInt(parsed.port, 10) : DEFAULT_PORTS[scheme];
  return { scheme, host, port };
}

/** The stable string an origin is stored and displayed under. */
function originKey(origin) {
  if (!origin) return null;
  const bare = origin.port === DEFAULT_PORTS[origin.scheme];
  return bare ? `${origin.scheme}://${origin.host}` : `${origin.scheme}://${origin.host}:${origin.port}`;
}

/**
 * Whether a parsed origin matches a config rule's pattern.
 *
 * The pattern arrives already normalised from the daemon
 * (`veld_core::ide::OriginPattern`), so this is comparison only — deliberately, so
 * that what `veld lint` accepted and what the matcher applies cannot drift apart.
 * `port: null` is the pattern's `*`; a pattern with a number matches that port
 * only.
 *
 * `wildcard` is a leading `*.` on the host, and the match is **label-wise**: the
 * host must end with `.` + the suffix. Never a bare `endsWith(suffix)` — that is
 * the version of this check that lets `evilveld.localhost` through for
 * `*.veld.localhost`. It also does not match the suffix itself: `*.veld.localhost`
 * is subdomains, and `veld.localhost` is written out if it is wanted.
 */
function matchesPattern(origin, pattern) {
  if (!origin || !pattern) return false;
  if (origin.scheme !== pattern.scheme) return false;
  const host = String(pattern.host).toLowerCase();
  const hostMatches = pattern.wildcard
    ? origin.host.endsWith(`.${host}`)
    : origin.host === host;
  if (!hostMatches) return false;
  return pattern.port === null || pattern.port === undefined || origin.port === pattern.port;
}

/**
 * The project config's answer for one permission at one origin, or `null`.
 *
 * `deny` wins over `allow` across every matching rule, not just within one: two
 * rules can match the same origin (`http://localhost:*` and
 * `http://localhost:3000`), and the safe reading of a config that says both
 * things is the restrictive one.
 */
function configVerdict(rules, origin, id) {
  if (!Array.isArray(rules) || !origin) return null;
  let allowed = false;
  for (const rule of rules) {
    if (!rule || !matchesPattern(origin, rule.origin)) continue;
    if (Array.isArray(rule.deny) && rule.deny.includes(id)) return "deny";
    if (Array.isArray(rule.allow) && rule.allow.includes(id)) allowed = true;
  }
  return allowed ? "allow" : null;
}

/**
 * Whether an origin names only this machine.
 *
 * Mirrors `veld_core::ide::is_local_origin`. Loopback and `.localhost` (RFC 6761
 * — resolvers must not send it to the network) are the origins a grant cannot
 * reach past the developer's own machine to serve.
 */
function isLocalOrigin(origin) {
  if (!origin) return false;
  const host = origin.host.replace(/^\[|\]$/g, "");
  return (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "::1" ||
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host)
  );
}

/**
 * Veld's own answer for one permission at one origin.
 *
 * `display-capture` is the conditional one: granted only at an origin veld itself
 * serves, because that is the case with no policy question in it. Veld's feedback
 * overlay captures the pane's own frame through `preferCurrentTab`, and the
 * frame is all the pane ever hands over — nothing is shared beyond what is
 * already on screen. At any other origin it prompts like everything else.
 */
function defaultVerdict(id, origin, trustedOrigins) {
  if (DEFAULT_ALLOW.has(id)) return "allow";
  if (id === "display-capture") {
    const key = originKey(origin);
    return key && Array.isArray(trustedOrigins) && trustedOrigins.includes(key) ? "allow" : "ask";
  }
  return "ask";
}

/**
 * Resolve one permission id.
 *
 * Precedence, highest first: the user's stored answer, the project config, then
 * veld's default. A user answer outranks the repo in *both* directions — someone
 * who denies the camera to a config-granted origin has to stay denied, or the
 * panel that offered them the switch was decorative.
 */
function resolveOne({ id, origin, stored, rules, trustedOrigins, inferred }) {
  const key = originKey(origin);
  const user = key && stored ? stored[key]?.[id] : undefined;
  if (user === "allow" || user === "deny") return { verdict: user, source: "user" };
  const fromConfig = configVerdict(rules, origin, id);
  if (fromConfig) return { verdict: fromConfig, source: "config" };
  // An inferred id never reaches the defaults — see `isInferred`.
  if (inferred) return { verdict: "ask", source: "default" };
  return { verdict: defaultVerdict(id, origin, trustedOrigins), source: "default" };
}

/**
 * The ids one Electron `media` permission covers.
 *
 * Electron reports camera and microphone as a single `media` permission and
 * distinguishes them only inside the details, while every browser's per-site UI
 * shows them as two switches. Splitting here is what lets the panel do the same.
 *
 * **A `media` *request* naming no type at all is `getDisplayMedia`.** Electron
 * populates `mediaTypes` only for *device* capture, so a screen-capture request
 * arrives as `media` with neither `video` nor `audio` — it does not arrive as
 * `display-capture`, despite that name existing in the request union. Treating the
 * empty case as a device enumeration and refusing it is what denied every
 * screenshot taken inside a pane, before `setDisplayMediaRequestHandler` was ever
 * consulted.
 *
 * A *check* is the opposite: `mediaType: "unknown"` there is
 * `enumerateDevices` asking whether labels may be shown, which is not access and
 * is not worth a prompt. Hence the `kind` argument — the same empty details mean
 * different things on the two handlers, and nothing else can tell them apart.
 */
function mediaIds(details, kind) {
  const types = new Set();
  if (details && Array.isArray(details.mediaTypes)) {
    for (const type of details.mediaTypes) types.add(type);
  }
  if (details && typeof details.mediaType === "string") types.add(details.mediaType);
  const ids = [];
  if (types.has("video")) ids.push("camera");
  if (types.has("audio")) ids.push("microphone");
  if (ids.length === 0 && kind === "request") return ["display-capture"];
  return ids;
}

/**
 * Whether the ids were *inferred* rather than stated by Electron.
 *
 * Only true for the empty-`mediaTypes` case above. It matters because that
 * inference is not provable — `MediaAccessPermissionRequest.mediaTypes` is
 * optional in Electron's typings, so "no types" is *probably* `getDisplayMedia`
 * and cannot be shown to be. Combined with the trusted-origin default-allow for
 * `display-capture`, a request veld could not identify would have been granted
 * silently at a veld URL with one `callback(true)` covering whatever it really
 * was — camera included. So an inferred id gets no *default*: it still honours an
 * explicit grant from the project or the user, and otherwise asks.
 */
function isInferred(electronName, details) {
  return electronName === "media" && mediaTypesOf(details).length === 0;
}

function mediaTypesOf(details) {
  const types = [];
  if (details && Array.isArray(details.mediaTypes)) types.push(...details.mediaTypes);
  if (details && typeof details.mediaType === "string") types.push(details.mediaType);
  return types.filter((t) => t === "video" || t === "audio");
}

/**
 * The veld ids an Electron permission name resolves to, or a fixed verdict.
 *
 * Returns `{ ids }` for anything the policy answers, or `{ verdict }` for the
 * names answered without consulting it. An unmapped name also returns a `deny`
 * verdict rather than throwing: a future Electron adding a permission must fail
 * closed, not crash the handler and leave the request hanging.
 */
function permissionIds(electronName, details, kind) {
  if (Object.hasOwn(FIXED_VERDICTS, electronName)) {
    return { verdict: FIXED_VERDICTS[electronName] };
  }
  if (electronName === "media") return { ids: mediaIds(details, kind) };
  const id = ELECTRON_TO_VELD[electronName];
  if (!id) return { verdict: "deny", unmapped: true };
  return { ids: [id] };
}

/**
 * The verdict for a whole Electron permission request.
 *
 * A request can cover more than one id (`media` with both camera and microphone),
 * and Electron takes one boolean for the pair. The combination is the strict one:
 * any deny denies, any ask asks, and it is allowed only when every id is. The
 * alternative — granting the half that was allowed — would hand a page a
 * microphone stream it never got a prompt for.
 */
function resolve({ electronName, details, origin, stored, rules, trustedOrigins, kind }) {
  const mapped = permissionIds(electronName, details, kind);
  if (mapped.verdict) {
    return { verdict: mapped.verdict, ids: [], source: mapped.unmapped ? "unmapped" : "fixed" };
  }
  const ids = mapped.ids;
  // Both are denials, and they are reported apart because they are different
  // bugs: one is a request veld could not attribute to a site, the other is one
  // that resolved to no permission veld models. Collapsing them into a single
  // "unattributable" sent the reader hunting for a missing origin that was
  // right there in the message.
  if (!origin) return { verdict: "deny", ids, source: "no-origin" };
  if (ids.length === 0) return { verdict: "deny", ids, source: "no-permission" };

  const inferred = isInferred(electronName, details);
  const parts = ids.map((id) => ({
    id,
    ...resolveOne({ id, origin, stored, rules, trustedOrigins, inferred }),
  }));
  const deny = parts.find((p) => p.verdict === "deny");
  if (deny) return { verdict: "deny", ids, source: deny.source, parts };
  const ask = parts.find((p) => p.verdict === "ask");
  if (ask) return { verdict: "ask", ids, source: ask.source, parts };
  return { verdict: "allow", ids, source: parts[0].source, parts };
}

/**
 * Every permission's current state at one origin — what the per-site panel shows.
 *
 * Includes the ids nobody has asked for yet, so the panel is a place to grant
 * something *before* hitting the feature that needs it, the way a browser's site
 * settings are. `source` is what lets the row say "set by veld.json" instead of
 * presenting a repo's decision as the user's own.
 */
function siteSettings({ origin, stored, rules, trustedOrigins }) {
  return VELD_PERMISSIONS.map((id) => {
    const { verdict, source } = resolveOne({ id, origin, stored, rules, trustedOrigins });
    return { id, verdict, source };
  });
}

/**
 * Store (or clear) a user's answer, returning a new store.
 *
 * Pure, and clears empty branches as it goes: a user who sets a permission back
 * to *Default* should leave no trace, or the file grows a row for every switch
 * anyone has ever toggled and "nothing is set here" becomes unrepresentable.
 */
function setAnswer(stored, partition, origin, id, verdict) {
  const key = originKey(origin);
  if (!key || !partition || !VELD_PERMISSIONS.includes(id)) return stored;
  const next = { ...stored };
  const forPartition = { ...(next[partition] ?? {}) };
  const forOrigin = { ...(forPartition[key] ?? {}) };
  if (verdict === "allow" || verdict === "deny") {
    forOrigin[id] = verdict;
  } else {
    delete forOrigin[id];
  }
  if (Object.keys(forOrigin).length === 0) delete forPartition[key];
  else forPartition[key] = forOrigin;
  if (Object.keys(forPartition).length === 0) delete next[partition];
  else next[partition] = forPartition;
  return next;
}

/**
 * Drop everything remembered for one session partition.
 *
 * A pane's session can already be cleared from the menu (cookies, storage, the
 * lot), and a permission grant that survived that would be the one piece of
 * "this site knows me" the clear silently missed.
 */
function forgetPartition(stored, partition) {
  if (!stored || !Object.hasOwn(stored, partition)) return stored;
  const next = { ...stored };
  delete next[partition];
  return next;
}

/**
 * The store to write, given what is on disk and what this process holds.
 *
 * Pure, and **here rather than beside the file I/O** for the reason the top of
 * this module gives: `browserViews.js` imports Electron, so nothing can load it
 * under `node --test`, and this exact logic was wrong twice in review — once
 * clobbering a second instance's answers, then once resurrecting revoked ones —
 * before anything could execute it.
 *
 * Merging is what stops two app instances (an unpackaged one beside the packaged
 * one, sharing a `userData`) from deleting each other's answers. Its cost is that
 * an *absence* means nothing: "never seen here" and "deleted here" are the same
 * shape, and a plain merge resolves both in favour of the file — which is
 * fail-**open**, because the thing most worth deleting is a grant.
 *
 * So deletions arrive explicitly. `revoked` is `partition\0origin\0id` for single
 * answers set back to Default; `cleared` is whole sessions signed out.
 */
function mergeForWrite(onDisk, inMemory, { revoked = [], cleared = [] } = {}) {
  const merged = { ...onDisk };
  for (const [partition, origins] of Object.entries(inMemory)) {
    merged[partition] = { ...(onDisk[partition] ?? {}) };
    for (const [origin, ids] of Object.entries(origins)) {
      merged[partition][origin] = { ...(onDisk[partition]?.[origin] ?? {}), ...ids };
    }
  }
  // A cleared session keeps only what *this* process has recorded since — the
  // file's pre-clear contents are dropped rather than the whole partition being
  // deleted. Deleting it meant the marker had to survive until the write landed,
  // and `persistPermissions` swallows write failures: a failed clear followed by
  // any later answer would merge every signed-out grant back. Fail-open, on the
  // one operation whose entire purpose is deletion.
  for (const partition of cleared) {
    merged[partition] = { ...(inMemory[partition] ?? {}) };
    if (Object.keys(merged[partition]).length === 0) delete merged[partition];
  }
  for (const key of revoked) {
    const [partition, origin, id] = key.split("\u0000");
    if (!merged[partition]?.[origin]) continue;
    // Copied before deleting: the spreads above are shallow, so without this the
    // `delete` reaches through into the caller's `onDisk` and silently corrupts
    // it. Harmless while the caller re-parses the file every time, and a trap for
    // the first person who caches that read.
    const ids = { ...merged[partition][origin] };
    delete ids[id];
    merged[partition] = { ...merged[partition] };
    if (Object.keys(ids).length === 0) delete merged[partition][origin];
    else merged[partition][origin] = ids;
    if (Object.keys(merged[partition]).length === 0) delete merged[partition];
  }
  return merged;
}

/** The key a single removed answer is recorded under. */
function revocationKey(partition, originKey, id) {
  return `${partition}\u0000${originKey}\u0000${id}`;
}

/**
 * Coerce a parsed permissions file into a store, dropping anything malformed.
 *
 * The file is written by a previous version of this app and read by this one, so
 * the only safe assumption is that it is JSON. An unrecognised verdict or a stray
 * key is dropped rather than kept: this file grants capabilities, and "we could
 * not read it, so we assumed allow" is not a failure mode worth having.
 */
function sanitizeStore(raw) {
  const out = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [partition, origins] of Object.entries(raw)) {
    if (!origins || typeof origins !== "object" || Array.isArray(origins)) continue;
    const cleanOrigins = {};
    for (const [origin, ids] of Object.entries(origins)) {
      if (!parseOrigin(origin) || !ids || typeof ids !== "object" || Array.isArray(ids)) continue;
      const cleanIds = {};
      for (const [id, verdict] of Object.entries(ids)) {
        if (!VELD_PERMISSIONS.includes(id)) continue;
        if (verdict !== "allow" && verdict !== "deny") continue;
        cleanIds[id] = verdict;
      }
      if (Object.keys(cleanIds).length > 0) cleanOrigins[origin] = cleanIds;
    }
    if (Object.keys(cleanOrigins).length > 0) out[partition] = cleanOrigins;
  }
  return out;
}

module.exports = {
  VELD_PERMISSIONS,
  mergeForWrite,
  revocationKey,
  isInferred,
  isLocalOrigin,
  ELECTRON_TO_VELD,
  FIXED_VERDICTS,
  DEFAULT_ALLOW,
  parseOrigin,
  originKey,
  matchesPattern,
  configVerdict,
  defaultVerdict,
  permissionIds,
  mediaIds,
  resolve,
  siteSettings,
  setAnswer,
  forgetPartition,
  sanitizeStore,
};
