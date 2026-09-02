/**
 * The safe-area inset payload, as `Emulation.setSafeAreaInsetsOverride` wants it.
 *
 * A file of its own for one function, because that function is the only *pure*
 * part of this feature's shell half and it is the part a plausible refactor
 * breaks silently. `browserViews.js` cannot be required without an Electron
 * runtime, so nothing in `npm test` reaches it — and the rule below ("send all
 * eight fields, every time, with each `Max` mirroring its inset") reads like
 * redundancy someone would tidy away. Here it has a test. The rest of the CDP
 * state machine — the attach accounting, the off-path revoke, the detach
 * bookkeeping — genuinely needs a running view and was verified by measurement
 * against Electron 43 instead; see `applyCdpNow`'s comments.
 *
 * @typedef {{top: number, right: number, bottom: number, left: number}} SafeAreaInsets
 */

/**
 * Build the `insets` argument for a set of gutters, or the reset for `null`.
 *
 * **All eight fields whenever there are any, never a subset.** Measured on
 * Electron 43: each call *replaces* the whole set rather than merging into it, so
 * sending `{top: 59}` alone leaves the other three at zero — and a call carrying
 * only an unrecognised key resets all four, because the unknown key is dropped
 * and the replacement still happens. There is therefore no partial update to be
 * had, and "off" is a call with an empty set rather than an omitted one.
 *
 * **Each `Max` mirrors its inset.** `env(safe-area-max-inset-*)` is the inset's
 * value with dynamic browser UI fully retracted, so on a real device it can only
 * be greater than or equal to the inset. An emulated viewport has no retracting
 * UI, which makes the inset already its own maximum. Omitting the `Max` fields
 * reports `0px` for a maximum while the inset itself reads `59px` — a combination
 * no handset can produce, and one that a page taking
 * `max(env(safe-area-inset-top), env(safe-area-max-inset-top))` reads as zero
 * headroom. Storing a second set of four numbers instead would buy exactly one
 * new expressible state, `max < inset`, which no hardware produces.
 *
 * @param {SafeAreaInsets|null} insets
 * @returns {Record<string, number>}
 */
function safeAreaPayload(insets) {
  if (insets === null) return {};
  return {
    top: insets.top,
    topMax: insets.top,
    right: insets.right,
    rightMax: insets.right,
    bottom: insets.bottom,
    bottomMax: insets.bottom,
    left: insets.left,
    leftMax: insets.left,
  };
}

module.exports = { safeAreaPayload };
