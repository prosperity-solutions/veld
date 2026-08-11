/**
 * Where a browser pane can go, as rows.
 *
 * This was `VeldLinks`, and it moved twice. It stopped being a pane kind of its own
 * (`services`) because the URLs are not a peer of a terminal and a page, they are how
 * you *get* a page. It has now also stopped being a list that sits *underneath* a
 * `Browser` button: a first user test had people press that button, land in a blank
 * pane, and never connect it to the URLs five rows below — two affordances for one
 * intent, at opposite ends of one screen. So the list is the affordance, and the
 * blank pane is one row of it ({@link PlaceList} `blank`).
 *
 * The other thing that test found: nobody could tell a run URL from a project
 * bookmark. They were the same row under two small captions. Now they are two row
 * shapes — a live dot and a service name against a bookmark glyph and a muted label —
 * because "veld started this and it is up" and "someone wrote this address in a
 * config" are different facts and only one of them is a claim veld can make.
 *
 * The same rows are the address bar's suggestions, which is what keeps typing and
 * picking from being two different lists of the same things.
 *
 * **The list is the run, and the bookmarks are one button.** A project with four to
 * eight services per run had the addresses it was serving *now* pushed below however
 * many bookmarks a config declared. `suggestionsFor` collapses them while nothing is
 * typed, and every surface offers them through {@link BookmarksModal} — the panel
 * here as a footer row, the two full-size screens as a control in their heading.
 */

import { ActionIcon, Modal, Tooltip } from "@mantine/core";
import {
  IconBookmark,
  IconCheck,
  IconCopy,
  IconExternalLink,
  IconSearch,
  IconWorld,
  IconWorldOff,
} from "@tabler/icons-react";
import { useState } from "react";
import type { Target } from "./model";
import type { Place, PlaceKind, Suggestions } from "./places";

/**
 * What each kind of place looks like — its group heading and its row glyph.
 *
 * A `Record<PlaceKind, …>`, so **adding a kind is a compile error here** rather than a
 * kind that silently renders as a bookmark.
 *
 * A run URL gets the live dot because veld started that server and knows it is up. A
 * bookmark cannot have one: it is a string in a config that nobody has probed, and a
 * green dot beside it would be a claim veld is in no position to make.
 *
 * The **heading** carries no mark of its own. It used to repeat the row glyph — a live
 * dot over "Running now", a bookmark glyph over "Project bookmarks" — on the theory
 * that the kinds should be legible before you read a row. Every row directly under it
 * already carries that same mark, so the heading's copy said nothing the next line did
 * not, and a second green dot two rows from the first read as a status of its own.
 */
const PLACE_KINDS: Record<PlaceKind, { heading: string; glyph: React.ReactNode }> = {
  run: {
    heading: "Running now",
    glyph: <span className="dot running" style={{ animation: "none" }} />,
  },
  bookmark: {
    heading: "Project bookmarks",
    glyph: <IconBookmark size={13} className="place-glyph" />,
  },
};

export function PlaceList(props: {
  /** Rows and the optional action row, from `suggestionsFor`. */
  suggestions: Suggestions;
  /**
   * The row the keyboard is on, or `-1`. Indexes {@link Suggestions} — action row
   * first — so it is the same arithmetic `pickSuggestion` does.
   */
  activeIndex?: number;
  /** Open this place, in the pane the list is being shown in. */
  onOpen: (url: string, title?: string) => void;
  /** Why there are no places, which only the app knows (no run, or no veld.json). */
  emptyHint: string;
  /**
   * Rows are a popup listbox for the address bar above them, not a page of links.
   *
   * Only the suggestion panel sets this. It is what makes `role="option"` and
   * `aria-selected` honest: a screen reader needs them when a field's arrow keys move
   * through the rows, and must not be told a start page's links are a widget's
   * options. `id` has to match the `aria-controls` the input names.
   */
  listboxId?: string;
}) {
  const { suggestions: s, activeIndex = -1, listboxId } = props;
  const rowId = (index: number) =>
    listboxId ? `${listboxId}-row-${index}` : undefined;
  return (
    <div
      className="place-list"
      id={listboxId}
      role={listboxId ? "listbox" : undefined}
    >
      {s.action && (
        <ActionRow
          action={s.action}
          active={activeIndex === 0}
          id={rowId(0)}
          asOption={listboxId !== undefined}
          onOpen={() => props.onOpen(s.action?.url ?? "")}
        />
      )}
      {s.places.map((place, i) => {
        const index = (s.action ? 1 : 0) + i;
        // A heading before the first row of each kind, emitted while walking rather
        // than by rendering two lists: the keyboard index has to run straight
        // through, and a filtered list can drop a whole kind.
        const heading =
          i === 0 || s.places[i - 1]?.kind !== place.kind ? place.kind : null;
        return (
          // `presentation` on the wrapper and the heading, in listbox mode: ARIA lets a
          // listbox own options and nothing else, and this wrapper exists only because
          // the heading walk needs somewhere to put a label. Without it the option
          // ownership — and the "3 of 7" a screen reader announces — is broken by the
          // very elements added to make the list legible.
          // The key carries the name as well as the URL, which **changes what two
          // bookmarks sharing one URL do**: `VeldLinks` keyed quicklinks by url alone
          // and said a repeated url "is the same link twice … and loses nothing by
          // collapsing". It did not really collapse — two children with one key is a
          // React warning and unspecified reconciliation, not a merge — and a config
          // that declares `Docs → /docs` twice under two labels is declaring two links.
          // So both render, each with a key of its own, and the pair is visible to the
          // author instead of silently half-dropped.
          <div
            key={`${place.kind}:${place.url}:${place.name}`}
            role={listboxId ? "presentation" : undefined}
          >
            {heading && (
              <span
                className="section-label"
                role={listboxId ? "presentation" : undefined}
              >
                {PLACE_KINDS[heading].heading}
              </span>
            )}
            <PlaceRow
              place={place}
              active={activeIndex === index}
              id={rowId(index)}
              asOption={listboxId !== undefined}
              onOpen={() => props.onOpen(place.url, place.name)}
            />
          </div>
        );
      })}
      {/* Two empty states, not one — and **neither belongs in the suggestion panel**,
          which is why both are gated on not being a listbox. `total === 0` is a fact
          about the *run* and only a start surface can helpfully explain it: rendered
          in the panel it put "start the run and its services appear here" in flow
          above somebody's documentation page, on every keystroke, which is a run-status
          lecture over unrelated content. And a "nothing matched" line makes no sense
          under an action row that plainly did match.
          Conflating the two in the first place printed the run hint over a *live* run
          with five URLs, because the query was narrower than the list. */}
      {!listboxId && s.total === 0 && (
        <div className="links-empty">
          <IconWorldOff size={26} />
          <p className="pane-screen-title">No URLs yet</p>
          <p className="faint">{props.emptyHint}</p>
        </div>
      )}
      {/* `narrowed`, because "nothing matched" is only true of a query. Without it,
          a project whose run has no URLs but which declares bookmarks — they are
          collapsed, so `places` is empty while `total` is not — was told its
          bookmarks did not match text nobody had typed. */}
      {!listboxId && s.narrowed && s.total > 0 && s.places.length === 0 && (
        <p className="faint place-nomatch">
          None of this project's URLs or bookmarks match what you typed.
        </p>
      )}
      {!listboxId && !s.narrowed && s.total > 0 && s.places.length === 0 && (
        <p className="faint place-nomatch">
          Nothing is running yet — this project's bookmarks are under Bookmarks.
        </p>
      )}
    </div>
  );
}

/**
 * The bookmarks control for the suggestion panel, which has no heading to put it in.
 *
 * A separate export rather than a footer row inside {@link PlaceList}, and the reason is
 * ARIA rather than taste: in the panel that list *is* the `role="listbox"` the address
 * bar names through `aria-controls`, and a listbox may own options and nothing else —
 * which is why the group headings in there carry `role="presentation"`. A `<button>`
 * dropped in as a bare child breaks that ownership and can turn up in the "3 of 7" a
 * screen reader announces. Rendered as a sibling of the list, it is simply a button.
 *
 * The two full-size surfaces do not use this — they have a heading, and put the same
 * control there beside a Blank browser button.
 */
export function BookmarksButton(props: { count: number; onOpen: () => void }) {
  return (
    <button type="button" className="btn place-bookmarks-row" onClick={props.onOpen}>
      <IconBookmark size={13} /> Bookmarks ({props.count})
    </button>
  );
}

/**
 * Every project bookmark, in a modal.
 *
 * One component for all three surfaces, because the modal *is* the bookmarks now:
 * they are no longer inline anywhere that nothing has been typed, so a second
 * rendering of them would be a second answer to "what did this config declare".
 *
 * A Mantine `Modal`, which is portalled — so `overlayGuard` hides every embedded
 * browser pane while it is open, and the rows are not painted over by a native view.
 * That is the whole reason this is allowed to be a modal when the suggestion panel is
 * not: the guard watches portals, and it cannot see a panel rendered inside a pane.
 */
export function BookmarksModal(props: {
  bookmarks: Place[];
  opened: boolean;
  onClose: () => void;
  /** Open this bookmark. The caller closes — where a bookmark goes differs per surface. */
  onOpen: (url: string, title?: string) => void;
}) {
  return (
    <Modal
      opened={props.opened}
      onClose={props.onClose}
      title="Project bookmarks"
      size="lg"
      centered
    >
      <div className="place-list bookmarks-modal-list">
        {props.bookmarks.length === 0 ? (
          <p className="faint place-nomatch">
            This project declares no bookmarks. They come from{" "}
            <code>ide.quicklinks</code> in veld.json.
          </p>
        ) : (
          props.bookmarks.map((place) => (
            <PlaceRow
              // Name as well as URL — two bookmarks may legitimately share a URL
              // under two labels; see the list's own key above.
              key={`${place.url}:${place.name}`}
              place={place}
              active={false}
              onOpen={() => props.onOpen(place.url, place.name)}
            />
          ))
        )}
      </div>
    </Modal>
  );
}

/**
 * The literal thing typed, as the first row.
 *
 * Named for what it will *do* rather than echoing the text back: "Search for react
 * hooks" is the sentence that answers "there is no URL in here, how do I use this?",
 * which is what a first-time user said out loud in front of a blank pane.
 */
function ActionRow(props: {
  action: Target;
  active: boolean;
  id?: string;
  asOption?: boolean;
  onOpen: () => void;
}) {
  const { action } = props;
  return (
    <button
      type="button"
      className="link-row place-action"
      id={props.id}
      role={props.asOption ? "option" : undefined}
      aria-selected={props.asOption ? props.active : undefined}
      data-active={props.active || undefined}
      // No hover handler. Hover used to write the keyboard's index, which meant that
      // after the pointer had crossed the list once, Enter opened the last row it
      // passed over instead of what was typed — and nothing cleared it, since only a
      // keystroke reset the index. Hover is a CSS state; the ring is what Enter will
      // do, and the two must not write to each other.
      onClick={props.onOpen}
    >
      {/* The same slot every place row's glyph sits in (`.place-mark`, which owns the
          width), so all three row shapes start their text on one vertical line. Without
          it the icon's own width set the offset, and a 7px live dot, a 13px bookmark and
          a 14px action icon put three text columns on one list.
          `IconWorld`, not `IconWorldWww`: the lettered globe reads as a graphic rather
          than an icon at 14px — its three glyphs collapse into a smudge and it sat
          visually lower than the search magnifier it alternates with. */}
      <span className="place-mark">
        {action.kind === "search" ? <IconSearch size={14} /> : <IconWorld size={14} />}
      </span>
      <span className="link-text">
        <span className="name">
          {action.kind === "search" ? `Search for ${action.query}` : "Go to"}
        </span>
        <span className="url">{action.url}</span>
      </span>
    </button>
  );
}

function PlaceRow(props: {
  place: Place;
  active: boolean;
  id?: string;
  asOption?: boolean;
  onOpen: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const { place } = props;
  return (
    <div
      className="link-row"
      id={props.id}
      // The frame carries the option role, not the inner button: it is what the ring
      // is drawn on and what `aria-activedescendant` points at. The copy and
      // open-externally controls are siblings inside it, which is why the row cannot
      // be a `<button>` itself.
      role={props.asOption ? "option" : undefined}
      aria-selected={props.asOption ? props.active : undefined}
      data-kind={place.kind}
      data-active={props.active || undefined}
    >
      {/* Siblings, not nested: a `<button>` inside a `<button>` is invalid HTML and
          browsers resolve it by dropping the inner one — the same rule the pane tabs
          follow. */}
      <button
        type="button"
        className="link-open"
        onClick={props.onOpen}
        title={`Open ${place.name} here`}
      >
        {/* From the one table, not a `kind === "run"` boolean: a third kind used to
            silently inherit the bookmark's glyph and its "someone wrote this in a
            config" framing. See `PLACE_KINDS`. The slot is fixed-width so a 7px dot
            and a 13px bookmark glyph leave their names on the same line. */}
        <span className="place-mark">{PLACE_KINDS[place.kind].glyph}</span>
        <span className="link-text">
          <span className="name">{place.name}</span>
          <span className="url">{place.url}</span>
        </span>
      </button>
      <Tooltip label={copied ? "Copied" : "Copy URL"} openDelay={250} withArrow>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label={`Copy the URL for ${place.name}`}
          onClick={() => {
            void navigator.clipboard.writeText(place.url);
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1200);
          }}
        >
          {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
        </ActionIcon>
      </Tooltip>
      <Tooltip label="Open in system browser" openDelay={250} withArrow>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          component="a"
          href={place.url}
          target="_blank"
          rel="noreferrer"
          aria-label={`Open ${place.name} in the system browser`}
        >
          <IconExternalLink size={13} />
        </ActionIcon>
      </Tooltip>
    </div>
  );
}
