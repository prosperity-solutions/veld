/**
 * The run's live URLs, as a launcher.
 *
 * This used to be a pane kind of its own (`services`), which was the wrong shape:
 * the URLs are not a peer of a terminal and a page, they are how you *get* a page.
 * A kind meant a singleton tab id, a "does one already exist" check at every call
 * site that could open it, and a second implementation of these rows. So it is a
 * component instead, shown in the two places you are about to need it: a `new`
 * pane, and a browser pane with no URL yet.
 *
 * Each row's primary action is opening the URL — that being what you almost always
 * want — with copy and open-externally as siblings rather than the row's only
 * affordances. Siblings, not nested: a `<button>` inside a `<button>` is invalid
 * HTML and browsers resolve it by dropping the inner one, the same rule the pane
 * tabs follow.
 */

import { ActionIcon, Tooltip } from "@mantine/core";
import { IconCheck, IconCopy, IconExternalLink, IconWorldOff } from "@tabler/icons-react";
import { useState } from "react";

export function VeldLinks(props: {
  urls: Array<[string, string]>;
  /** Why there are none, which only the app knows (no run, or no veld.json). */
  emptyHint: string;
  /** Open this URL — in the pane the list is being shown in. */
  onOpen: (name: string, url: string) => void;
}) {
  if (props.urls.length === 0) {
    return (
      <div className="links-empty">
        <IconWorldOff size={26} />
        <p className="pane-screen-title">No URLs yet</p>
        <p className="faint">{props.emptyHint}</p>
      </div>
    );
  }
  return (
    <div className="links-list">
      <span className="section-label">Veld URLs</span>
      {props.urls.map(([name, url]) => (
        <LinkRow key={name} name={name} url={url} onOpen={() => props.onOpen(name, url)} />
      ))}
      {props.urls.length > 1 && (
        <button
          className="btn links-all"
          onClick={() => props.urls.forEach(([, url]) => window.open(url, "_blank"))}
        >
          <IconExternalLink size={13} /> Open all in system browser
        </button>
      )}
    </div>
  );
}

function LinkRow(props: { name: string; url: string; onOpen: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="link-row">
      <button
        type="button"
        className="link-open"
        onClick={props.onOpen}
        title={`Open ${props.name} here`}
      >
        <span className="dot running" style={{ animation: "none" }} />
        <span className="link-text">
          <span className="name">{props.name}</span>
          <span className="url">{props.url}</span>
        </span>
      </button>
      <Tooltip label={copied ? "Copied" : "Copy URL"} openDelay={250} withArrow>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label={`Copy the URL for ${props.name}`}
          onClick={() => {
            void navigator.clipboard.writeText(props.url);
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
          href={props.url}
          target="_blank"
          rel="noreferrer"
          aria-label={`Open ${props.name} in the system browser`}
        >
          <IconExternalLink size={13} />
        </ActionIcon>
      </Tooltip>
    </div>
  );
}
