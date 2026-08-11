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
  onHover?: (index: number) => void;
  /** Why there are no places, which only the app knows (no run, or no veld.json). */
  emptyHint: string;
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
   * Open every run URL in the system browser. Passed by the two start surfaces and
   * deliberately not by the suggestion panel over a live page: there, the list is
   * answering "where should this pane go", and a control that opens six other
   * windows is not an answer to it.
   */
  onOpenAll?: () => void;
}) {
  const { suggestions: s, activeIndex = -1 } = props;
  const nothing = s.count === 0;
  return (
    <div className="place-list">
      {s.action && (
        <ActionRow
          action={s.action}
          active={activeIndex === 0}
          onOpen={() => props.onOpen(s.action?.url ?? "")}
          onHover={() => props.onHover?.(0)}
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
              onOpen={() => props.onOpen(place.url, place.name)}
              onHover={() => props.onHover?.(index)}
            />
          </div>
        );
      })}
      {nothing && (
        <div className="links-empty">
          <IconWorldOff size={26} />
          <p className="pane-screen-title">No URLs yet</p>
          <p className="faint">{props.emptyHint}</p>
        </div>
      )}
      {/* Two or more, as before: "open all" for one URL is the row above it. */}
      {props.onOpenAll && s.places.filter((p) => p.kind === "run").length > 1 && (
        <button type="button" className="btn links-all" onClick={props.onOpenAll}>
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
  onOpen: () => void;
  onHover: () => void;
}) {
  const { action } = props;
  return (
    <button
      type="button"
      className="link-row place-action"
      data-active={props.active || undefined}
      onClick={props.onOpen}
      onMouseMove={props.onHover}
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
  onOpen: () => void;
  onHover: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const { place } = props;
  const live = place.kind === "run";
  return (
    <div
      className="link-row"
      data-kind={place.kind}
      data-active={props.active || undefined}
      onMouseMove={props.onHover}
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
