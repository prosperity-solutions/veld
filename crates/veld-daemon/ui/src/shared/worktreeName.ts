/**
 * Turning what someone types into the two names a worktree actually needs.
 *
 * The create dialog is **name-first**: you type a label for the checkout and the
 * alias and branch are derived from it. That is the inverse of `default_alias`
 * (branch → alias) in `veld-core`, and the inverse is not symmetric, so it lives
 * here rather than being assumed to be the same function backwards:
 *
 * - **The alias has to be an identifier.** `validate_alias` accepts 1–64 characters
 *   of letters, digits, `-`, `_` and `.` — no spaces — because the alias is also the
 *   default *run* name for that checkout, and a run name is addressed from a shell
 *   (`veld start --name …`) and slugified into a hostname. So `Checkout V2 (final)`
 *   cannot be stored verbatim; it becomes `checkout-v2-final`.
 * - **The branch may keep its slashes.** `feat/Checkout V2` is a branch name with a
 *   conventional prefix, and flattening the `/` into a `-` would quietly rename
 *   someone's convention. Each segment is slugged, the separators survive.
 *
 * Both derivations are lossy, which is exactly why the dialog renders their output
 * *before* creating anything: a silent rename is what makes a name field
 * untrustworthy. Nothing here is a validator — the daemon still decides — and
 * nothing here guesses at uniqueness, which is per repo and compared as slugs.
 */

/** Longest derived name. Matches `slugify`'s cap in `veld-core`, which is what the
 *  hostname for a worktree's run is built with — a longer alias would be truncated
 *  there and two aliases could collapse into one host. */
export const MAX_DERIVED_LEN = 48;

/** Longest accepted display name. Mirrors `MAX_DISPLAY_NAME_LEN` in
 *  `crates/veld-daemon/src/desktop.rs` — a courtesy bound so the dialog can stop
 *  you before the daemon does, never the enforcement. Larger than
 *  [`MAX_DERIVED_LEN`] because it is bounded by what fits a rail column, not by
 *  what fits a hostname label. */
export const MAX_DISPLAY_NAME_LEN = 80;

/**
 * Letters that carry meaning a bare `-` would throw away.
 *
 * German `ä ö ü ß` have *established* ASCII spellings — `ae oe ue ss` — and a German
 * speaker typing `Zahlungsübersicht` expects `zahlungsuebersicht`, not
 * `zahlungs-bersicht`. Everything else here is the ordinary "drop the accent" rule
 * (`é ê ë è → e`), which is what every other Latin-script language does when it has
 * to reach for ASCII. Nordic `æ ø å` and `œ` follow their own conventions
 * (`ae oe aa oe`), matching how those languages transliterate themselves.
 *
 * The table only holds what a plain `NFD`-plus-strip-the-marks pass gets *wrong*.
 * That pass is right for `é ê ë ç ñ š ž`, and wrong for German: `ü` is not "u with a
 * decoration" there, and decomposition would silently give `u`. So the table runs
 * first and the generic pass mops up afterwards — see [`transliterate`]. A table is
 * also the only form in which the German rule and the French rule can disagree,
 * which they must.
 *
 * Scope, stated: Latin script only. A name in Cyrillic or Greek still slugs to
 * nothing and the dialog says so — transliterating those needs per-language tables
 * and a dependency, and the honest answer for now is to let the user type a name.
 */
const TRANSLITERATE: Record<string, string> = {
  // German. `ae oe ue ss` is the established spelling and the reason this table
  // exists at all — the generic rule below would give `a o u` and `s`.
  ä: "ae",
  ö: "oe",
  ü: "ue",
  ß: "ss",
  // Nordic and other ligatures, which have their own conventions and mostly do not
  // decompose to ASCII + a mark, so the generic rule cannot reach them.
  æ: "ae",
  œ: "oe",
  ø: "oe",
  å: "aa",
  // Letters whose ASCII form is a stroke or a digraph rather than an accent.
  ł: "l",
  đ: "d",
  ð: "d",
  þ: "th",
  ħ: "h",
  ŋ: "ng",
  ı: "i",
};

/**
 * Fold a name to ASCII letters: table first, then plain accent removal.
 *
 * Three steps, each load-bearing:
 *
 * 1. **Lowercase, then `NFC`.** Lowercasing first means the table needs no capital
 *    entries (the slug is lowercase anyway) — the first version had only lowercase
 *    keys *without* this, and `Ñandú` came out as `andu`. Composing to NFC is what
 *    makes a macOS-typed `ä` (which arrives as `a` + U+0308) hit the German entry
 *    instead of falling through to step 3 and becoming `a`.
 * 2. **The table**, for letters whose ASCII spelling is not "the same letter minus
 *    its accent".
 * 3. **Decompose and drop combining marks**, which covers every remaining accented
 *    Latin letter — `é ê ë ě ç ñ š ž ř ğ` — without a table entry each. Deliberately
 *    *after* the table, so German never reaches it.
 */
function transliterate(text: string): string {
  let out = "";
  for (const ch of text.toLowerCase().normalize("NFC")) {
    out += TRANSLITERATE[ch] ?? ch;
  }
  return out.normalize("NFD").replace(/[\u0300-\u036f]/g, "");
}

/** Slug one segment: lowercase alphanumerics, every other run collapsed to `-`. */
function slugSegment(text: string): string {
  let out = "";
  let dash = true; // suppresses a leading dash
  for (const ch of transliterate(text)) {
    if (/[a-zA-Z0-9]/.test(ch)) {
      out += ch.toLowerCase();
      dash = false;
    } else if (!dash) {
      out += "-";
      dash = true;
    }
  }
  return out.replace(/-+$/, "");
}

/**
 * Slug the way `veld_core::url::slugify` does — **without** transliteration.
 *
 * The one place this matters is comparing a derived alias against the aliases a repo
 * already has. Those are stored strings, and the daemon decides collisions with
 * `slugify`, which has no transliteration table: to it, `café` is `caf`. If this
 * client transliterated a *stored* alias it would compute `cafe`, disagree with the
 * daemon in both directions, and either warn about a collision that will not happen
 * or stay quiet about one that will.
 */
function storedSlug(alias: string): string {
  let out = "";
  let dash = true;
  for (const ch of alias) {
    if (/[a-zA-Z0-9]/.test(ch)) {
      out += ch.toLowerCase();
      dash = false;
    } else if (!dash) {
      out += "-";
      dash = true;
    }
  }
  return out.replace(/-+$/, "");
}

/**
 * The alias a typed name becomes: one slug, capped, `""` when nothing survives.
 *
 * `""` is not an error message, it is the signal that there is nothing to create —
 * the dialog disables its button on it. (`default_alias` substitutes `"wt"` in the
 * same situation, deliberately not copied: that fallback exists for a branch
 * discovered on disk, which must get *some* row, whereas a person typing `///` into
 * a name field has not chosen `wt`.)
 */
export function deriveAlias(name: string): string {
  return slugSegment(name).slice(0, MAX_DERIVED_LEN).replace(/-+$/, "");
}

/**
 * The branch a typed name becomes: each `/`-separated segment slugged, empty
 * segments dropped, capped as a whole.
 *
 * The cap applies to the joined string and can therefore land mid-segment; the
 * trailing `-` and `/` cleanup is what keeps the result a legal ref rather than
 * something git refuses (`a/b/` and `a//b` are both invalid).
 */
export function deriveBranch(name: string): string {
  const joined = name
    .split("/")
    .map(slugSegment)
    .filter((s) => s !== "")
    .join("/");
  return joined.slice(0, MAX_DERIVED_LEN).replace(/[-/]+$/, "");
}

/**
 * The one answer to "what is this worktree called on screen".
 *
 * The row carries two names and they are not interchangeable: `alias` is the
 * identifier (bounded, unique per repo, the default run name, and therefore part
 * of a hostname), `display_name` is what the user typed. Every *label* — a rail
 * row, a menu item, a window title, a toast — goes through this; every *key* —
 * the run name, a collision check, an API argument — keeps using `alias`
 * directly.
 *
 * A function rather than `w.display_name || w.alias` at forty call sites, because
 * the fallback is the part that gets forgotten: a surface that reads
 * `display_name` raw renders an empty string for every worktree created before
 * v13, and it looks fine in a dev database where you just typed a name.
 */
export function worktreeLabel(w: {
  alias: string;
  /** Optional on purpose: a UI served by a daemon older than v13 sends no such
   *  key at all, and `just dev-ui` proxies `/api` to whatever daemon is
   *  installed. Both `undefined` and `""` mean "render the alias". */
  display_name?: string;
}): string {
  return w.display_name || w.alias;
}

/**
 * The display name a typed create-dialog name becomes: trimmed, inner runs of
 * whitespace collapsed, capped.
 *
 * Lossless in the way that matters — capitals, punctuation and non-ASCII all
 * survive, which is the entire reason this field exists next to the alias. Only
 * whitespace is normalised, because `Hello   test` and `Hello test` are the same
 * name and one of them renders with a hole in it.
 */
export function deriveDisplayName(name: string): string {
  // Sliced by **code point**, not by `String.prototype.slice`. That slices UTF-16
  // code units, so a cap landing inside a surrogate pair leaves a lone high
  // surrogate — `JSON.stringify` emits it as `\ud83d` and serde_json rejects the
  // whole request body, so an emoji-heavy name failed as an unparseable payload
  // rather than as a name that was too long. The daemon's own cap counts
  // characters, so this also makes the two bounds mean the same thing.
  return [...name.trim().replace(/\s+/g, " ")]
    .slice(0, MAX_DISPLAY_NAME_LEN)
    .join("")
    .trim();
}

/**
 * Whether `alias` collides with one of `taken`, compared as slugs.
 *
 * Slug comparison, matching `Db::patch_worktree` and `unique_alias`: a worktree's
 * hostname is `slugify(alias)`, so `main-2` and `main_2` are one name to the router
 * even though they are two strings. This is a courtesy check — it lets the dialog
 * say so before the button is pressed — and never the enforcement, which stays in
 * the daemon's transaction where a concurrent create can be seen.
 *
 * Deliberately about the **alias** and not the display name: two checkouts may
 * carry the same label, because a label collides in nothing.
 */
export function aliasCollides(alias: string, taken: string[]): boolean {
  // `alias` is already a derived (ASCII) value, so either slug function agrees on it;
  // the siblings are stored strings and must go through `storedSlug`. See its note.
  const slug = storedSlug(alias);
  if (slug === "") return false;
  return taken.some((t) => storedSlug(t) === slug);
}
