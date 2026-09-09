// What a right-click inside a browser pane offers.
//
// **Native, not DOM, and that is the whole design.** A `WebContentsView` paints
// over every piece of app UI regardless of z-index, so drawing this menu in
// `/ide` would mean going through `panes/overlayGuard.ts` — capture the page to a
// still, hide the live view, draw over the freeze, and put the view back on
// dismiss (see the *Native-view z-order* row in `desktop/ARCHITECTURE.md`). That
// is an acceptable price for an address-bar dropdown that stays open while you
// type. It is the wrong price for a context menu: the gesture is "click, read,
// click", the page would visibly freeze and thaw around it, and a menu at the
// pointer is the one surface where a frame of stutter is the whole experience.
// An OS menu costs none of that, arrives at the pointer for free, and is what
// this pane is pretending to be anyway.
//
// The template is built here, pure, so the item set is testable without an
// Electron binary — `browserViews.js` owns only the mapping from an item id to
// the `webContents` call that performs it. `MENU_IDS` is exported so a test can
// hold the two halves together; nothing else ties them, and the dispatch's
// `default: break` turns a mismatch into a row that silently does nothing.
//
// **Two things Chromium reports that this menu deliberately does not use**, both
// recorded because the omission otherwise reads as an oversight:
//
//   * **Spellcheck.** `webPreferences` does not set `spellcheck`, and Electron
//     defaults it to `true`, so a pane's text fields really do underline
//     misspellings — and `params.misspelledWord` / `params.dictionarySuggestions`
//     are right there. Offering them means suggestion rows plus *Add to
//     Dictionary* (`session.addWordToSpellCheckerDictionary`), which is a feature
//     with its own surface, not a row. It is a real gap, named as a follow-up
//     rather than pretended away.
//   * **Everything a download would do** — *Save Link As*, *Save Image As*,
//     *Print*. A pane is for looking at a dev server, and a shell that starts
//     writing files out of arbitrary web content is a different trust
//     conversation than this menu is having.

/**
 * Codepoints an address must never carry to the clipboard.
 *
 * Everything invisible or reordering, and the list is exhaustive of what the
 * class contains rather than a summary of it: C0 controls, `DEL` and the C1
 * controls, the soft hyphen, the Arabic letter mark, the Hangul Choseong and
 * Jungseong fillers, the Mongolian vowel separator, the zero-width and
 * joiner family (including the left- and right-to-left marks), LINE and
 * PARAGRAPH SEPARATOR, the bidi embedding and override controls, the word joiner
 * and the invisible math operators, the bidi isolates, the Hangul filler, the
 * BOM, the interlinear annotation marks, the halfwidth Hangul filler, and the
 * astral musical format controls. The `u` flag is what lets that last range be
 * written as a codepoint rather than a surrogate pair.
 *
 * **Why an allow-list of *visibility* rather than of characters.** The user
 * clicked *Copy Email Address* and will paste the result trusting it is the
 * address they saw. A `mailto:` is page-controlled, and `u.pathname` hands it
 * over still percent-encoded — so `mailto:a%0Ab@example.com` is inert until
 * `decodeURIComponent` unwraps it into a real newline, and
 * `mailto:%E2%80%AEmoc.elpmaxe@a` unwraps into an address that *renders*
 * backwards. `.trim()` runs first and strips these only at the ends; an interior
 * one is what this class is for.
 *
 * Deliberately not the printable-ASCII test `safeUserAgent` uses: an
 * internationalised address (`üser@exämple.de`, `田中@example.jp`) is legal under
 * SMTPUTF8, and refusing one would be a bug of its own. What is refused is
 * invisibility, not non-ASCII.
 *
 * Every range here was reproduced leaking through an earlier, narrower version of
 * this class — U+2028, U+2029, U+061C, U+180E, U+3164 and U+FFF9 all reached the
 * clipboard before a review round caught them. Widen it rather than trusting that
 * the obvious ones are the only ones.
 */
const UNSAFE_IN_ADDRESS =
  /[\u0000-\u001f\u007f-\u009f\u00ad\u061c\u115f\u1160\u180e\u200b-\u200f\u2028\u2029\u202a-\u202e\u2060-\u2064\u2066-\u2069\u3164\ufeff\ufff9-\ufffb\uffa0]|[\u{1d173}-\u{1d17a}]/u;

/**
 * RFC 5321's ceiling: a 64-**octet** local part, `@`, and a 255-octet domain.
 *
 * Measured with `Buffer.byteLength`, not `String.length`. The two agree only on
 * ASCII: `String.length` counts UTF-16 code units, so a 310-character CJK address
 * is 315 units and **935 octets** — it would pass a `.length` check three times
 * over, which is what an earlier version of this did.
 */
const MAX_ADDRESS_OCTETS = 320;

/**
 * The address inside a `mailto:` link, or `null` when it carries none — or when
 * what it carries is not safe to hand over as an address.
 *
 * Copying `mailto:someone@example.com?subject=Hi` verbatim is technically the
 * link's address and is never what the user wanted — the point of the gesture is
 * to get an address into a To: field. Percent-decoded, because a `mailto:` in the
 * wild encodes the local part (`first%2Blast@example.com`).
 *
 * **Rejected rather than repaired**, following `safeUserAgent` in
 * `validate.js`: an address with a newline in it is not an address with a typo.
 * And rejecting is the clean answer here rather than merely the safe one, because
 * `null` makes the builder fall back to *Copy Link Address*, which copies
 * Chromium's own still-encoded `linkURL` — where a smuggled `%0A` stays visibly
 * `%0A` and inert. The user loses nothing but the convenience.
 *
 * A `mailto:?subject=x` has a recipient list of nothing, which is a real thing
 * pages emit for "compose a mail to yourself" widgets. That returns `null` too.
 */
function mailtoAddress(raw) {
  if (typeof raw !== "string") return null;
  let u;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "mailto:") return null;
  let address;
  try {
    address = decodeURIComponent(u.pathname);
  } catch {
    // A malformed escape (`%zz`) throws rather than decoding. The raw recipient
    // list is still closer to an address than the whole `mailto:` URL is — and it
    // goes through the same two checks below, so nothing skips the guard.
    address = u.pathname;
  }
  address = address.trim();
  if (address === "" || Buffer.byteLength(address, "utf8") > MAX_ADDRESS_OCTETS) return null;
  if (UNSAFE_IN_ADDRESS.test(address)) return null;
  return address;
}

/** Whether this link is one the shell will open in a pane — see `safeUrl`. */
function isOpenableLink(raw) {
  if (typeof raw !== "string") return false;
  try {
    const u = new URL(raw);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * What *Copy … Address* is called for each kind of element that has one.
 *
 * `srcURL` is populated for images, audio and video (`ContextMenuParams` in
 * `electron.d.ts` says so in as many words), so all three get the item and the
 * label names what the user right-clicked. `canvas`, `file` and `plugin` are the
 * remaining `mediaType` values and carry no `srcURL`, so they fall through to the
 * page menu — a canvas has bytes Chromium could copy, but nothing here can
 * confirm what it reports for `hasImageContents`, so it is left alone rather than
 * guessed at.
 */
// `Object.create(null)`, not `{}`, because the lookup below is keyed by a value
// that arrives from outside: `MEDIA_ADDRESS_LABEL["toString"]` on an object
// literal returns a *function*, which becomes an item whose `label` is a function
// and which `Menu.buildFromTemplate` then rejects. Unreachable from Chromium's
// closed `mediaType` enum, but this module is exported as an independently
// callable pure unit and the drift gate cannot see it — the id emitted is a real
// one. A prototype-free map removes the class of bug rather than the instance.
const MEDIA_ADDRESS_LABEL = Object.assign(Object.create(null), {
  image: "Copy Image Address",
  video: "Copy Video Address",
  audio: "Copy Audio Address",
});

/** Shared, and frozen because it is pushed by reference into every template. */
const SEPARATOR = Object.freeze({ separator: true });

/**
 * The menu for one `context-menu` event, as `{id, label, enabled}` rows and
 * `{separator: true}` breaks.
 *
 * **The target wins; navigation is the fallback.** Chrome's page menu carries
 * Back/Forward/Reload and Chrome's *link* menu carries neither, and that is not
 * an oversight to correct: a menu raised on a link is answering "what about this
 * link", and four items you did not aim at push the one you did further from the
 * pointer. So the nav group appears only when nothing else was under the cursor.
 *
 * **The rule has three named exceptions, and they are exceptions rather than
 * bugs.** A `canvas`, a `file` and a `plugin` target all *were* under the cursor
 * and still get the page menu, because none of them carries a `srcURL` and so
 * there is no address item to offer. A canvas does have bytes Chromium could
 * copy; whether it reports `hasImageContents` for one is not something that could
 * be observed while this was written, and an unverifiable branch is worse than a
 * named gap. `browserMenu.test.js` pins all three, so the fall-through is a
 * decision the tests hold rather than an accident.
 *
 * @param {import('electron').ContextMenuParams} params
 * @param {{canGoBack?: boolean, canGoForward?: boolean, canInspect?: boolean}} caps
 */
function contextMenuItems(params, caps) {
  // `?? {}` on both, not a default parameter: a default only fires for
  // `undefined`, and an explicit `null` — which is what a caller reading these
  // off an optional record hands over — would reach the property access and
  // throw inside a native event handler, i.e. a right-click that raises no menu.
  const p = params ?? {};
  const c = caps ?? {};
  const flags = p.editFlags ?? {};
  const items = [];

  const link = typeof p.linkURL === "string" && p.linkURL !== "" ? p.linkURL : null;
  const email = link ? mailtoAddress(link) : null;
  const mediaLabel = MEDIA_ADDRESS_LABEL[p.mediaType];
  const media = mediaLabel && typeof p.srcURL === "string" && p.srcURL !== "" ? p.mediaType : null;
  const selection = typeof p.selectionText === "string" && p.selectionText.trim() !== "";

  if (link) {
    if (isOpenableLink(link)) {
      items.push({ id: "open-link", label: "Open Link in New Tab", enabled: true });
    }
    items.push(
      email
        ? { id: "copy-email", label: "Copy Email Address", enabled: true }
        : { id: "copy-link", label: "Copy Link Address", enabled: true },
    );
  }

  if (media) {
    if (items.length > 0) items.push(SEPARATOR);
    // Only an image has bytes this menu can put on the clipboard — there is no
    // `copyVideoAt`. So audio and video get the address item alone.
    if (media === "image") {
      // *Copy Image* and *Copy Image Address* are not the same question, and
      // Chromium answers them separately. `srcURL` is an address, which a broken
      // `<img>` still has; `hasImageContents` is whether there are decoded bytes.
      // Copying the address of an image that failed to load is useful — that is
      // how you find out *what* failed — so only the bitmap item is greyed.
      items.push({ id: "copy-image", label: "Copy Image", enabled: p.hasImageContents !== false });
    }
    items.push({ id: "copy-media-address", label: mediaLabel, enabled: true });
  }

  if (p.isEditable) {
    if (items.length > 0) items.push(SEPARATOR);
    // Greyed rather than absent, because a text field's menu is a fixed shape
    // everywhere else: an item that comes and goes with the selection makes
    // Paste land at a different height each time you open it. Undo/Redo are part
    // of that shape — Chrome, Safari and Firefox all open a text field's menu
    // with them — so leaving them out would have made the rule above false.
    //
    // **Two polarities, and the difference is deliberate.** `=== true` means a
    // missing flag reads as "the field cannot do this", which is the safe
    // direction for an action that changes the page. `!== false` on Select All
    // means it survives a `params` with no `editFlags` at all, because selecting
    // is harmless and an inert Select All is worse than an optimistic one.
    items.push({ id: "undo", label: "Undo", enabled: flags.canUndo === true });
    items.push({ id: "redo", label: "Redo", enabled: flags.canRedo === true });
    items.push(SEPARATOR);
    items.push({ id: "cut", label: "Cut", enabled: flags.canCut === true });
    items.push({ id: "copy", label: "Copy", enabled: flags.canCopy === true });
    items.push({ id: "paste", label: "Paste", enabled: flags.canPaste === true });
    items.push(SEPARATOR);
    items.push({ id: "select-all", label: "Select All", enabled: flags.canSelectAll !== false });
  } else if (selection) {
    if (items.length > 0) items.push(SEPARATOR);
    // `enabled: true`, not `flags.canCopy === true`: outside an editable field
    // Chromium does not always populate `editFlags`, and there is demonstrably a
    // selection — `selectionText` is what got us into this branch.
    items.push({ id: "copy", label: "Copy", enabled: true });
  }

  if (items.length === 0) {
    items.push({ id: "back", label: "Back", enabled: c.canGoBack === true });
    items.push({ id: "forward", label: "Forward", enabled: c.canGoForward === true });
    items.push({ id: "reload", label: "Reload", enabled: true });
    items.push(SEPARATOR);
    items.push({ id: "select-all", label: "Select All", enabled: flags.canSelectAll !== false });
  }

  if (c.canInspect === true) {
    items.push(SEPARATOR);
    items.push({ id: "inspect", label: "Inspect Element", enabled: true });
  }

  return items;
}

/**
 * Every id `contextMenuItems` can emit.
 *
 * Exported so `browserMenu.test.js` can hold this list and
 * `dispatchContextMenuAction`'s `switch` in `browserViews.js` together. Nothing else
 * does: they are two independent lists in two files, and the `switch`'s
 * `default: break` makes a drift between them a menu row that silently does
 * nothing rather than an error anybody sees.
 */
const MENU_IDS = Object.freeze([
  "open-link",
  "copy-link",
  "copy-email",
  "copy-image",
  "copy-media-address",
  "undo",
  "redo",
  "cut",
  "copy",
  "paste",
  "select-all",
  "back",
  "forward",
  "reload",
  "inspect",
]);

module.exports = { contextMenuItems, mailtoAddress, isOpenableLink, MENU_IDS };
