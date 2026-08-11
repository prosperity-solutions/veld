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
 */

import { ActionIcon, Tooltip } from "@mantine/core";
import {
  IconBookmark,
  IconCheck,
  IconCopy,
  IconExternalLink,
  IconSearch,
  IconWindow,
  IconWorldOff,
  IconWorldWww,
} from "@tabler/icons-react";
import { useState } from "react";
import type { Target } from "./model";
import type { Place, Suggestions } from "./places";

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
  /**
   * Offer a blank pane as the last row. The chooser passes this — it is how a pane
   * becomes a browser with nothing loaded, which is what you want for reading
   * something that is not one of your own URLs. Omitted on a pane that is *already*
   * blank, where the row would do nothing.
   */
  onBlank?: () => void;
  /** Whether search is configured, which changes what the blank row promises. */
  canSearch?: boolean;
  /**
   * Open these URLs in the system browser. Passed by the two start surfaces and
   * deliberately not by the suggestion panel over a live page: there, the list is
   * answering "where should this pane go", and a control that opens six other
   * windows is not an answer to it.
   *
   * **The rows decide which URLs, not the caller.** Both callers used to close over
   * the run's whole URL set while the button was gated on the *filtered* rows, so a
   * query narrowing six services to two put "open all" under two rows and opened six
   * windows.
   */
  onOpenAll?: (urls: string[]) => void;
}) {
  const { suggestions: s, activeIndex = -1, listboxId } = props;
  const runUrls = s.places.filter((p) => p.kind === "run").map((p) => p.url);
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
          <div key={`${place.kind}:${place.url}:${place.name}`}>
            {heading === "run" && (
              <span className="section-label">
                <span className="dot running" style={{ animation: "none" }} />
                Running now
              </span>
            )}
            {heading === "bookmark" && (
              <span className="section-label">
                <IconBookmark size={11} />
                Project bookmarks
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
      {/* Two empty states, not one. `total === 0` is a fact about the *run* and only
          the app can explain it; a filter that matched nothing is a fact about what
          was typed. Conflating them printed "start the run and its services appear
          here" over a live run with five URLs, because the query was narrower than
          the list. */}
      {s.total === 0 && (
        <div className="links-empty">
          <IconWorldOff size={26} />
          <p className="pane-screen-title">No URLs yet</p>
          <p className="faint">{props.emptyHint}</p>
        </div>
      )}
      {s.total > 0 && s.places.length === 0 && (
        <p className="faint place-nomatch">Nothing here matches what you typed.</p>
      )}
      {/* Two or more, as before: "open all" for one URL is the row above it. */}
      {props.onOpenAll && runUrls.length > 1 && (
        <button
          type="button"
          className="btn links-all"
          onClick={() => props.onOpenAll?.(runUrls)}
        >
          <IconExternalLink size={13} /> Open all in system browser
        </button>
      )}
      {props.onBlank && (
        <button type="button" className="place-blank" onClick={props.onBlank}>
          <IconWindow size={15} />
          <span className="link-text">
            <span className="name">Blank browser</span>
            <span className="url">
              {props.canSearch
                ? "Type any address, or search"
                : "Type any address"}
            </span>
          </span>
        </button>
      )}
    </div>
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
      {action.kind === "search" ? (
        <IconSearch size={14} />
      ) : (
        <IconWorldWww size={14} />
      )}
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
  const live = place.kind === "run";
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
        {/* A green dot beside an address nobody has probed would be a claim veld
            cannot make, so a bookmark gets a glyph saying what it is instead. */}
        {live ? (
          <span className="dot running" style={{ animation: "none" }} />
        ) : (
          <IconBookmark size={13} className="place-glyph" />
        )}
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
