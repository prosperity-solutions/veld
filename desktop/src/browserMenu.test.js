const assert = require("node:assert/strict");
const test = require("node:test");

const { contextMenuItems, mailtoAddress, isOpenableLink, MENU_IDS } = require("./browserMenu.js");

/** Item ids in order, with separators as `"-"`, which is what the shape assertions read. */
const shape = (items) => items.map((i) => (i.separator ? "-" : i.id));

/** The row with this id, or `undefined`. */
const row = (items, id) => items.find((i) => i.id === id);

const CAPS = { canGoBack: false, canGoForward: false, canInspect: true };

test("an empty page offers navigation, and nothing that needs a target", () => {
  const items = contextMenuItems({ editFlags: { canSelectAll: true } }, CAPS);
  assert.deepEqual(shape(items), ["back", "forward", "reload", "-", "select-all", "-", "inspect"]);
  // Both disabled: a first page load has no history either way, and the items are
  // shown greyed rather than omitted so the menu keeps one shape.
  assert.equal(row(items, "back").enabled, false);
  assert.equal(row(items, "forward").enabled, false);
  assert.equal(row(items, "reload").enabled, true);
});

test("history enables the items it exists for", () => {
  const items = contextMenuItems({}, { ...CAPS, canGoBack: true, canGoForward: true });
  assert.equal(row(items, "back").enabled, true);
  assert.equal(row(items, "forward").enabled, true);
});

test("an http link offers opening and copying, and drops the nav group", () => {
  // The target wins: a menu raised on a link answers about the link, so
  // Back/Forward/Reload are not in it — pushing the item you aimed at four rows
  // further from the pointer is the cost this rule avoids.
  const items = contextMenuItems({ linkURL: "https://example.com/a?b=c#d" }, CAPS);
  assert.deepEqual(shape(items), ["open-link", "copy-link", "-", "inspect"]);
});

test("a mailto link copies the address, not the URL, and is not openable", () => {
  const items = contextMenuItems({ linkURL: "mailto:someone@example.com?subject=Hi" }, CAPS);
  // No `open-link`: the shell refuses every non-http(s) scheme, so offering it
  // would be an item that does nothing.
  assert.deepEqual(shape(items), ["copy-email", "-", "inspect"]);
});

test("a mailto with no recipient falls back to copying the link", () => {
  // Real pages emit this for "compose a message" widgets. A *Copy Email Address*
  // here would copy an empty string.
  const items = contextMenuItems({ linkURL: "mailto:?subject=Feedback" }, CAPS);
  assert.deepEqual(shape(items), ["copy-link", "-", "inspect"]);
});

test("a tel link is copyable but not openable", () => {
  const items = contextMenuItems({ linkURL: "tel:+41791234567" }, CAPS);
  assert.deepEqual(shape(items), ["copy-link", "-", "inspect"]);
});

test("an image offers its bytes and its address", () => {
  const items = contextMenuItems(
    { mediaType: "image", srcURL: "https://example.com/logo.png", hasImageContents: true },
    CAPS,
  );
  assert.deepEqual(shape(items), ["copy-image", "copy-media-address", "-", "inspect"]);
  assert.equal(row(items, "copy-image").enabled, true);
});

test("an image that failed to load keeps its address and greys its bytes", () => {
  // How you find out *what* failed is by copying the address, so that item stays
  // live; there is nothing on the clipboard to put for the other one.
  const items = contextMenuItems(
    { mediaType: "image", srcURL: "https://example.com/gone.png", hasImageContents: false },
    CAPS,
  );
  assert.deepEqual(shape(items), ["copy-image", "copy-media-address", "-", "inspect"]);
  assert.equal(row(items, "copy-image").enabled, false);
  assert.equal(row(items, "copy-media-address").enabled, true);
});

test("a linked image offers both groups, separated", () => {
  const items = contextMenuItems(
    {
      linkURL: "https://example.com/article",
      mediaType: "image",
      srcURL: "https://example.com/hero.png",
    },
    CAPS,
  );
  assert.deepEqual(shape(items), [
    "open-link",
    "copy-link",
    "-",
    "copy-image",
    "copy-media-address",
    "-",
    "inspect",
  ]);
});

test("a media element with no source is not a media target", () => {
  // An `<img>` whose URL failed to resolve still reports `mediaType: "image"`;
  // with no `srcURL` there is nothing for either item to act on. (An earlier
  // version of this comment claimed a `<video>` poster frame lands here — it does
  // not: `electron.d.ts` gives a video element `mediaType: "video"`, poster
  // included, which the next test covers.)
  const items = contextMenuItems({ mediaType: "image", srcURL: "" }, CAPS);
  assert.deepEqual(shape(items), ["back", "forward", "reload", "-", "select-all", "-", "inspect"]);
  for (const kind of ["video", "audio"]) {
    assert.deepEqual(
      shape(contextMenuItems({ mediaType: kind, srcURL: "" }, CAPS)),
      ["back", "forward", "reload", "-", "select-all", "-", "inspect"],
      kind,
    );
  }
});

test("a video or an audio element offers its address, and no bitmap item", () => {
  // There is no `copyVideoAt`, so only an image gets the bytes item — but
  // Chromium populates `srcURL` for all three, and "copy that address" was the
  // stated use case. Before this, a right-click on a `<video>` fell through to the
  // *page* menu, which looks like it worked and answers a question nobody asked.
  const video = contextMenuItems({ mediaType: "video", srcURL: "https://x/v.mp4" }, CAPS);
  assert.deepEqual(shape(video), ["copy-media-address", "-", "inspect"]);
  assert.equal(video[0].label, "Copy Video Address");

  const audio = contextMenuItems({ mediaType: "audio", srcURL: "https://x/a.mp3" }, CAPS);
  assert.deepEqual(shape(audio), ["copy-media-address", "-", "inspect"]);
  assert.equal(audio[0].label, "Copy Audio Address");
});

test("canvas, file and plugin targets fall through to the page menu", () => {
  // Deliberate, not forgotten: none carries a `srcURL`, and a canvas's
  // `hasImageContents` is not something this run could confirm.
  for (const kind of ["canvas", "file", "plugin"]) {
    assert.deepEqual(
      shape(contextMenuItems({ mediaType: kind, srcURL: "" }, CAPS)),
      ["back", "forward", "reload", "-", "select-all", "-", "inspect"],
      kind,
    );
  }
});

test("a selection offers Copy only", () => {
  const items = contextMenuItems({ selectionText: "the marked words" }, CAPS);
  assert.deepEqual(shape(items), ["copy", "-", "inspect"]);
  assert.equal(row(items, "copy").enabled, true);
});

test("whitespace is not a selection", () => {
  // A click that lands between two words reports a selection of a newline or a
  // space. *Copy* on it is an item that appears to do nothing.
  const items = contextMenuItems({ selectionText: "  \n " }, CAPS);
  assert.deepEqual(shape(items), ["back", "forward", "reload", "-", "select-all", "-", "inspect"]);
});

test("a text field keeps one shape, greying what it cannot do", () => {
  const empty = contextMenuItems(
    { isEditable: true, editFlags: { canCut: false, canCopy: false, canPaste: true } },
    CAPS,
  );
  assert.deepEqual(shape(empty), [
    "undo",
    "redo",
    "-",
    "cut",
    "copy",
    "paste",
    "-",
    "select-all",
    "-",
    "inspect",
  ]);
  assert.equal(row(empty, "cut").enabled, false);
  assert.equal(row(empty, "copy").enabled, false);
  assert.equal(row(empty, "paste").enabled, true);

  const selected = contextMenuItems(
    {
      isEditable: true,
      selectionText: "typed",
      editFlags: { canCut: true, canCopy: true, canPaste: true },
    },
    CAPS,
  );
  // Same rows, same order — only the enablement moves.
  assert.deepEqual(shape(selected), shape(empty));
  assert.equal(row(selected, "cut").enabled, true);
});

test("a read-only field still offers Copy rather than Cut", () => {
  // `readonly`/`disabled` inputs report `isEditable: false`, so they take the
  // selection branch and never show a Cut that Chromium would refuse.
  const items = contextMenuItems({ isEditable: false, selectionText: "read only" }, CAPS);
  assert.deepEqual(shape(items), ["copy", "-", "inspect"]);
});

test("a link inside a text field shows the link items above the edit items", () => {
  const items = contextMenuItems(
    {
      linkURL: "https://example.com/",
      isEditable: true,
      editFlags: { canCut: true, canCopy: true, canPaste: true },
    },
    CAPS,
  );
  assert.deepEqual(shape(items), [
    "open-link",
    "copy-link",
    "-",
    "undo",
    "redo",
    "-",
    "cut",
    "copy",
    "paste",
    "-",
    "select-all",
    "-",
    "inspect",
  ]);
});

test("without DevTools the menu ends at its last real item", () => {
  // No trailing separator: a menu whose last row is a divider is a menu with a
  // missing item, and that is what a naive unconditional push would render.
  const items = contextMenuItems({ selectionText: "x" }, { canInspect: false });
  assert.deepEqual(shape(items), ["copy"]);
  const page = contextMenuItems({}, {});
  assert.deepEqual(shape(page), ["back", "forward", "reload", "-", "select-all"]);
});

test("no params at all still produces a usable menu", () => {
  // Defence in depth: the builder is handed Chromium's report, and a shape it did
  // not expect must not throw inside a native event handler.
  assert.deepEqual(shape(contextMenuItems(undefined)), [
    "back",
    "forward",
    "reload",
    "-",
    "select-all",
  ]);
  assert.deepEqual(shape(contextMenuItems(null, null)), [
    "back",
    "forward",
    "reload",
    "-",
    "select-all",
  ]);
});

test("mailtoAddress decodes the local part and drops the query", () => {
  assert.equal(mailtoAddress("mailto:someone@example.com"), "someone@example.com");
  assert.equal(
    mailtoAddress("mailto:someone@example.com?subject=Hi&body=There"),
    "someone@example.com",
  );
  // A `+` alias is routinely percent-encoded in a `mailto:`, and the point of the
  // item is to get something pasteable into a To: field.
  assert.equal(mailtoAddress("mailto:first%2Blast@example.com"), "first+last@example.com");
  assert.equal(mailtoAddress("mailto:a@b.com,c@d.com"), "a@b.com,c@d.com");
});

test("mailtoAddress refuses everything that is not a mailto with a recipient", () => {
  assert.equal(mailtoAddress("mailto:"), null);
  assert.equal(mailtoAddress("mailto:?subject=x"), null);
  assert.equal(mailtoAddress("mailto:%20"), null);
  assert.equal(mailtoAddress("https://example.com/"), null);
  assert.equal(mailtoAddress("not a url"), null);
  assert.equal(mailtoAddress(""), null);
  assert.equal(mailtoAddress(undefined), null);
});

test("mailtoAddress survives a malformed escape rather than throwing", () => {
  // `decodeURIComponent("%zz")` throws. Inside a menu handler that would be a
  // right-click that raises no menu at all.
  assert.equal(mailtoAddress("mailto:a%zz@example.com"), "a%zz@example.com");
});

test("isOpenableLink allows only what the shell will navigate to", () => {
  assert.equal(isOpenableLink("http://example.com/"), true);
  assert.equal(isOpenableLink("https://example.com/"), true);
  assert.equal(isOpenableLink("mailto:a@b.com"), false);
  assert.equal(isOpenableLink("tel:+41791234567"), false);
  assert.equal(isOpenableLink("javascript:alert(1)"), false);
  assert.equal(isOpenableLink("file:///etc/passwd"), false);
  assert.equal(isOpenableLink("data:text/html,hi"), false);
  assert.equal(isOpenableLink("/relative"), false);
  assert.equal(isOpenableLink(undefined), false);
});

test("a javascript: link is copyable but never offered for opening", () => {
  // Copying is text into the clipboard, which is what every browser does here.
  // Opening is a navigation, and the scheme filter is the gate for those.
  const items = contextMenuItems({ linkURL: "javascript:alert(1)" }, CAPS);
  assert.deepEqual(shape(items), ["copy-link", "-", "inspect"]);
});

test("a text field opens with Undo and Redo, greyed to the field's own flags", () => {
  // Chrome, Safari and Firefox all lead a text field's menu with these, so the
  // "fixed shape" rule the module claims is only true if they are here.
  const items = contextMenuItems(
    { isEditable: true, editFlags: { canUndo: true, canRedo: false, canPaste: true } },
    CAPS,
  );
  assert.equal(row(items, "undo").enabled, true);
  assert.equal(row(items, "redo").enabled, false);
  // A field with no flags at all greys both rather than offering an Undo that
  // would throw away the page's state on a guess.
  const bare = contextMenuItems({ isEditable: true }, CAPS);
  assert.equal(row(bare, "undo").enabled, false);
  assert.equal(row(bare, "redo").enabled, false);
  // Select All is the deliberate exception to that polarity: harmless, so it
  // survives a `params` carrying no `editFlags`.
  assert.equal(row(bare, "select-all").enabled, true);
});

test("mailtoAddress refuses an address carrying anything invisible", () => {
  // All four reproduced against this module before the guard existed: a page can
  // pre-encode them, and `decodeURIComponent` is what unwraps them into the real
  // codepoint on its way to the clipboard.
  assert.equal(mailtoAddress("mailto:a%0Ab@example.com"), null, "interior newline");
  assert.equal(mailtoAddress("mailto:a%0Db@example.com"), null, "interior CR");
  assert.equal(mailtoAddress("mailto:%E2%80%AEmoc.elpmaxe@a"), null, "RTL override");
  assert.equal(mailtoAddress("mailto:a%E2%80%8Bb@example.com"), null, "zero-width space");
  assert.equal(mailtoAddress("mailto:a%00b@example.com"), null, "NUL");
  assert.equal(mailtoAddress("mailto:a%C2%ADb@example.com"), null, "soft hyphen");
  assert.equal(mailtoAddress("mailto:a%EF%BB%BFb@example.com"), null, "interior BOM");
});

test("trim runs before the guard, so a leading BOM is cleaned rather than refused", () => {
  // Deliberate ordering, asserted because it is surprising: JS `trim()` counts
  // U+FEFF as whitespace, so a *leading* or trailing BOM is stripped and what is
  // left is a perfectly good address — refusing it would cost the user the item
  // for no gain. U+200B is **not** whitespace to `trim()`, so a leading
  // zero-width space is refused instead of cleaned. Interior is refused either
  // way, which is the case the guard exists for.
  assert.equal(mailtoAddress("mailto:%EF%BB%BFa@example.com"), "a@example.com");
  assert.equal(mailtoAddress("mailto:%20%20a@example.com"), "a@example.com");
  assert.equal(mailtoAddress("mailto:%E2%80%8Ba@example.com"), null);
});

test("a rejected address falls back to Copy Link Address, which is inert", () => {
  // The fallback is the point of rejecting rather than stripping: the user still
  // gets something, and what they get is Chromium's own `linkURL` where the
  // smuggled codepoint is still percent-encoded and therefore visible.
  const items = contextMenuItems({ linkURL: "mailto:a%0Ab@example.com" }, CAPS);
  assert.deepEqual(shape(items), ["copy-link", "-", "inspect"]);
});

test("mailtoAddress bounds the length at RFC 5321's ceiling", () => {
  const local = "a".repeat(64);
  const domain = `${"b".repeat(59)}.example.com`;
  assert.equal(mailtoAddress(`mailto:${local}@${domain}`), `${local}@${domain}`);
  assert.equal(mailtoAddress(`mailto:${"a".repeat(5000)}@example.com`), null);
});

test("mailtoAddress still accepts an internationalised address", () => {
  // The guard rejects invisibility, not non-ASCII — `safeUserAgent`'s
  // printable-ASCII test would have been wrong here, because SMTPUTF8 addresses
  // are legal and refusing one would be a bug of its own.
  assert.equal(mailtoAddress("mailto:%C3%BCser@ex%C3%A4mple.de"), "üser@exämple.de");
  assert.equal(mailtoAddress("mailto:%E7%94%B0%E4%B8%AD@example.jp"), "田中@example.jp");
});

test("every id the builder can emit is dispatched in browserViews.js", () => {
  // The drift gate. `MENU_IDS`, `contextMenuItems` and `runContextMenuAction`'s
  // switch are three lists in two files with nothing tying them together, and the
  // switch's `default: break` makes a mismatch a menu row that silently does
  // nothing — no error, no log, nothing anybody sees. So: assert the builder
  // emits nothing outside `MENU_IDS`, and that the dispatch has a `case` for
  // every entry. Reading the sibling file as text is crude, but `browserViews.js`
  // requires `electron` and cannot be loaded under `node --test`.
  //
  // **Sliced to `dispatchContextMenuAction`'s body first, and that is not
  // tidiness.** Searching the whole file false-*passed* for `back`, `forward` and
  // `reload`: the unrelated `veld:browser:command` switch carries the identical
  // three `case` labels, so all three could be deleted from the menu dispatch and
  // this test would still be green. A gate with three fake assertions in it is
  // worse than no gate, because it is believed.
  const fs = require("node:fs");
  const path = require("node:path");
  const source = fs.readFileSync(path.join(__dirname, "browserViews.js"), "utf8");

  const marker = "function dispatchContextMenuAction(";
  const from = source.indexOf(marker);
  assert.notEqual(from, -1, "dispatchContextMenuAction not found — did it get renamed?");
  // The function ends at the first `}` in column zero after it, which is the
  // file's own formatting convention throughout.
  const to = source.indexOf("\n}\n", from);
  assert.notEqual(to, -1, "could not find the end of dispatchContextMenuAction");
  const body = source.slice(from, to);

  // The slice has to be the real thing, or every assertion below is vacuous.
  assert.ok(body.includes("switch (id)"), "slice does not contain the dispatch switch");
  assert.ok(!body.includes("veld:browser:command"), "slice leaked into the IPC handler");

  for (const id of MENU_IDS) {
    assert.ok(
      body.includes(`case "${id}":`) || body.includes(`case "${id}": {`),
      `dispatchContextMenuAction has no case for menu id "${id}" — add one there, or drop the id from MENU_IDS in browserMenu.js`,
    );
  }

  // And the other direction: sweep every branch of the builder and check nothing
  // it produces is missing from `MENU_IDS`.
  const everyShape = [
    {},
    { linkURL: "https://example.com/" },
    { linkURL: "mailto:a@b.com" },
    { linkURL: "tel:+1" },
    { mediaType: "image", srcURL: "https://x/i.png" },
    { mediaType: "video", srcURL: "https://x/v.mp4" },
    { mediaType: "audio", srcURL: "https://x/a.mp3" },
    { selectionText: "x" },
    { isEditable: true, editFlags: { canCut: true, canCopy: true, canPaste: true } },
  ];
  const seen = new Set();
  for (const params of everyShape) {
    for (const item of contextMenuItems(params, CAPS)) {
      if (!item.separator) seen.add(item.id);
    }
  }
  for (const id of seen) {
    assert.ok(MENU_IDS.includes(id), `builder emits "${id}", which is not in MENU_IDS`);
  }
  // The sweep is only a gate if it actually reaches every id.
  assert.deepEqual([...seen].sort(), [...MENU_IDS].sort(), "sweep did not cover every MENU_ID");
});

test("the drift gate would actually fail if a case went missing", () => {
  // A gate nobody has seen fail is a gate nobody should believe — this is the
  // meta-test that the slicing above is load-bearing. It takes the real dispatch
  // body, removes the `back` case the way a careless edit would, and confirms the
  // assertion the gate makes no longer holds. Without the slice this passed
  // anyway, because `veld:browser:command` supplies the same label.
  const fs = require("node:fs");
  const path = require("node:path");
  const source = fs.readFileSync(path.join(__dirname, "browserViews.js"), "utf8");
  const from = source.indexOf("function dispatchContextMenuAction(");
  const body = source.slice(from, source.indexOf("\n}\n", from));

  const withoutBack = body.replace('case "back":', "");
  assert.ok(!withoutBack.includes('case "back":'), "meta-test removed nothing");
  // The whole file, by contrast, still contains it — which is the false pass.
  assert.ok(
    source.replace('case "back":', "").includes('case "back":'),
    "the second `case \"back\":` in browserViews.js is gone; the slice is now belt-and-braces rather than load-bearing, and this meta-test can go",
  );
});

test("isOpenableLink never promises a link safeUrl will refuse", () => {
  // Two independent scheme allow-lists — this one and `safeUrl` in `validate.js`
  // — with nothing but agreement between them. They agree today; if a later edit
  // widens this one (adding `mailto:` is the natural wrong move, since the
  // comment says "see `safeUrl`" rather than "must match"), the menu grows an
  // *Open Link in New Tab* that the dispatch's own `safeUrl` call then refuses:
  // a row that silently does nothing, which is exactly what `MENU_IDS`'s gate
  // exists to prevent one file over. `validate.js` is pure and Electron-free, so
  // this can be a real assertion rather than a comment.
  const { safeUrl } = require("./validate.js");
  const corpus = [
    "http://example.com/",
    "https://example.com/a?b=c#d",
    "https://user:pw@example.com:8443/x",
    "mailto:a@b.com",
    "tel:+41791234567",
    "sms:+41791234567",
    "javascript:alert(1)",
    "data:text/html,hi",
    "file:///etc/passwd",
    "blob:https://example.com/uuid",
    "ftp://example.com/x",
    "chrome://settings",
    "about:blank",
    "/relative",
    "not a url",
    "",
  ];
  for (const raw of corpus) {
    if (isOpenableLink(raw)) {
      assert.notEqual(
        safeUrl(raw),
        null,
        `isOpenableLink says "${raw}" is openable but safeUrl refuses it — the menu would offer a row that does nothing`,
      );
    }
  }
  // And the gate must not be vacuous: something in the corpus has to be openable.
  assert.ok(corpus.some(isOpenableLink), "corpus contains no openable link");
});
