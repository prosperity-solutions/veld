/**
 * The colour Chromium paints where the guest document paints nothing.
 *
 * A file of its own for one expression, for the same reason `safeArea.js` is:
 * `browserViews.js` cannot be required without an Electron runtime, so nothing
 * in `npm test` reaches it — and this rule is a two-branch conditional that
 * reads like an inconsistency somebody would tidy into a single constant. Both
 * branches are load-bearing, and tidying either one away is a visible bug that
 * only shows up in a packaged app.
 *
 * Two demands pull in opposite directions:
 *
 * **Before anything has committed** the view is an empty rectangle, and it should
 * be the app's own theme surface. Electron's default white is a flash in a dark
 * app — on every view create and every navigation to a slow page — and that
 * flash is where an embedded view stops looking embedded.
 *
 * **Once a page has committed**, white. A document that declares no background
 * of its own is *transparent*, so this colour is what the reader actually sees
 * behind the text — and the UA stylesheet colours that text black, because
 * Chromium's own base background there is white. Painting a dark theme surface
 * underneath instead gave a plain no-CSS page black text on a near-black
 * background: unreadable, and shown by no real browser.
 *
 * What this deliberately does **not** do is force a plain page dark to match the
 * app. That looks better — Chromium's auto-dark, or injecting
 * `color-scheme: light dark` — but it means the pane rendering something the
 * developer's real browser does not, which is the one thing a preview pane must
 * not do. The choice was put to the maintainer as exactly that trade and this is
 * the answer: be faithful.
 *
 * Two residues, stated so the next reader does not treat either as a bug.
 *
 * **One paint.** A page slow to *render* — committed, not yet painted — now
 * flashes white in a dark app, where before it flashed nothing. That window is
 * one paint, against the whole of a load before.
 *
 * **The emulated screen's corners, permanently.** `setBorderRadius` clips the
 * native view to a rounded rect, and what shows in the corners it clips away is
 * the DOM element behind it — `.browser-device-frame`, painted `--term-bg` in
 * `styles.css`. So on a dark theme a committed plain page is now light with dark
 * corner notches, where before both sides were the surface and the seam was
 * invisible.
 *
 * That seam is **not new**, and this is why it is documented rather than chased:
 * any page whose own background is not the theme surface already had it — a
 * white-styled page has shown dark corners on a dark theme for as long as device
 * emulation has existed. This change only moves *plain* pages from one side of
 * it to the other. Making the frame white to match would fix them and break
 * every dark-styled page, which currently matches; the only real fix is sampling
 * the committed page's own background, which is a different feature. Do not
 * "fix" this by coupling the frame's colour to the commit signal.
 *
 * The "has committed" fact is `browserViews.js`'s `entry.frameReady`, read
 * directly rather than renamed here: it is the same flag device emulation gates
 * on (rule 2 in that file's header — `enableDeviceEmulation` on a view with no
 * frame segfaults the app), reused rather than restated, and reset by
 * `render-process-gone`, correctly, since a dead renderer is an empty rectangle
 * again. Both features want exactly "has a main frame ever committed since the
 * last renderer death", so the sharing is deliberate; a future change to that
 * flag's meaning has to answer to both.
 *
 * @param {{surface: string, frameReady: boolean}} entry
 * @returns {string} a CSS hex colour
 */
function baseBackground(entry) {
  return entry.frameReady ? "#ffffff" : entry.surface;
}

module.exports = { baseBackground };
