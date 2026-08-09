/**
 * Node actions, raised to the top bar and the new-pane chooser.
 *
 * A run's nodes each declare a handful of actions (a restart, an admin route,
 * a one-off command — whatever the project's config exposes). They used to live
 * only on the node cards in the Nodes view, which is a pane you have to open:
 * the actions for the run that is *currently driving the IDE* were one click
 * away from the surface that is always up. This is the same set of buttons, in
 * two contexts that are always visible — the top bar (as a menu) and the
 * new-pane chooser (as a section) — so a running project's actions are no
 * longer behind a pane you have to find.
 *
 * `nodes` arrives pre-filtered to those that actually carry actions: a node
 * with none has no business in a menu built for acting. The caller also decides
 * whether to render at all (no live run, or none of its nodes can act), since
 * that is contextual rather than a preference.
 */

import { useState } from "react";
import { Button } from "@mantine/core";

import { api, type RunRef } from "../api";
import type { NodeRow } from "./NodeList";
import { notifyError } from "./notify";

/**
 * The grouped action buttons themselves, ready to embed wherever they should
 * appear. Each node is its own group, so a menu or section stays legible when a
 * run has several nodes with overlapping action labels.
 *
 * Manages its own per-action busy state — the two mounts of this component (top
 * bar and chooser) never render for the same action at the same time, so they
 * do not need to share it.
 */
export function NodeActions(props: {
  run: RunRef;
  nodes: NodeRow[];
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  return (
    <>
      {props.nodes.map((n) => (
        <div key={n.name} className="node-action-group">
          <span className="node-action-node">{n.name}</span>
          <div className="node-actions">
            {n.actions.map((a) => (
              <Button
                key={a.name}
                size="compact-xs"
                variant="default"
                loading={busy === `${n.name}:${a.name}`}
                onClick={() => {
                  setBusy(`${n.name}:${a.name}`);
                  api
                    .runAction(props.run, a.name, n.name)
                    .then(() => props.onChanged())
                    .catch((e) => notifyError(`${a.label} on ${n.name}`, e))
                    .finally(() => setBusy(null));
                }}
              >
                {a.label}
              </Button>
            ))}
          </div>
        </div>
      ))}
    </>
  );
}
