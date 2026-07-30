/**
 * Sharing, as one implementation for both modes.
 *
 * Runs mode hangs these on an environment card; IDE mode hangs them on a
 * top-level Sharing surface in the top bar plus a join-request banner (#152: a
 * top-level surface, not a relay-details dump). Everything here is UI over
 * endpoints that already exist (`/api/shares`, `…/mode`,
 * `…/requests/{id}/approve|deny`) — no new plumbing, which is also why forking
 * it would have been pure duplication.
 *
 * A share is attached to its run by `run_id`, never by name: two repos both
 * checked out on `main` have two environments called `main`, and a name-keyed
 * filter hangs the other project's share on this run.
 */

import { Badge, Button, Checkbox, Group, Text, Tooltip } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import { api, type PendingInfo, type RunRef, type ShareInfo } from "../api";
import { useCopyFlash } from "./copy";
import { notifyDone, notifyError } from "./notify";

/**
 * This run's shares: the peer share (at most one) and any public web shares.
 *
 * A share is attached by `run_id`, never by name (see the module comment), and the
 * peer/web split is "has no public URLs" — the same rule runs mode has always used.
 *
 * **Known ambiguity, and the wire cannot resolve it.** `POST /api/shares?web`
 * inserts the share and *then* registers it with the gateway
 * (`crates/veld-daemon/src/share/api.rs`), and every field that would give a web
 * share away — `public_urls`, `web_password`, and even `ticket`/`join_url` — is
 * derived from that registration (`share/manager.rs`). So between the two, a
 * pending web share is byte-identical to a peer share and is classified as one:
 * the Share button hides and "Stop sharing" points at it. The window is one HTTP
 * round-trip, it closes by itself, and the fix belongs daemon-side (register before
 * inserting, or mark the record) — tracked as a follow-up. `sharing.test.ts` pins
 * the current behaviour as a canary.
 */
export function sharesForRun(
  shares: ShareInfo[],
  runId: string,
): { peer: ShareInfo | null; web: ShareInfo[] } {
  const mine = shares.filter((s) => s.run_id === runId);
  return {
    peer: mine.find((s) => s.public_urls.length === 0) ?? null,
    web: mine.filter((s) => s.public_urls.length > 0),
  };
}

/**
 * The run a share was minted from.
 *
 * A join request names only its `share_id`, so this is what lets a prompt say
 * *which* environment someone is asking to join. Display only — attaching a share
 * to a run is `run_id`'s job (see the module comment).
 */
export function runOfShare(shares: ShareInfo[], shareId: string): string | null {
  return shares.find((s) => s.id === shareId)?.run ?? null;
}

/**
 * Run a share mutation, reporting failure as a toast and refreshing after.
 *
 * A hook rather than a helper because every surface needs the same in-flight
 * label for its button spinners. `context` names the attempt, since the daemon's
 * refusals ("run 'x' has no services opted into peer sharing…") do not say which
 * control produced them.
 */
function useShareAction(onChanged: () => void) {
  const [busy, setBusy] = useState<string | null>(null);
  // Whether this component is still mounted — a share can be started from a
  // popover that closes while the request is in flight. Re-armed in the effect
  // body, not only initialised: StrictMode mounts, cleans up and mounts again, so
  // a flag only set at declaration would stay false for the whole dev session and
  // leave every button spinning.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const act = async (label: string, context: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await fn();
      onChanged();
    } catch (e) {
      notifyError(context, e);
    } finally {
      if (alive.current) setBusy(null);
    }
  };
  return { busy, act };
}

/** Per-connection transport badges — the reason a slow share is diagnosable. */
export function ConnBadges(props: { share: ShareInfo }) {
  return (
    <>
      {props.share.connections.map((c, i) => {
        const who = c.label || c.node_id.slice(0, 10);
        const rtt = c.rtt_ms != null ? ` ${c.rtt_ms}ms` : "";
        if (c.transport === "direct") {
          return (
            <Badge key={i} size="xs" color="green" variant="light">
              {who}: direct{rtt}
            </Badge>
          );
        }
        if (c.transport === "relayed") {
          return (
            <Tooltip key={i} label="Throughput is limited by the relay">
              <Badge size="xs" color="yellow" variant="light">
                {who}: relayed via {c.via ?? "?"}
                {rtt}
              </Badge>
            </Tooltip>
          );
        }
        return (
          <Badge key={i} size="xs" color="gray" variant="light">
            {who}: no open path
          </Badge>
        );
      })}
    </>
  );
}

/**
 * Start/stop controls for a run's shares.
 *
 * Starting a peer share copies the join link straight to the clipboard — that is
 * the entire point of starting one — and confirms it with a toast, because the
 * button that was clicked is replaced by "Stop sharing" in the same breath and so
 * cannot carry the confirmation itself.
 */
export function ShareControls(props: {
  run: RunRef;
  /** Sharing needs a live run; the daemon refuses otherwise. */
  running: boolean;
  peer: ShareInfo | null;
  web: ShareInfo[];
  onChanged: () => void;
}) {
  const { busy, act } = useShareAction(props.onChanged);

  return (
    <>
      {props.running && !props.peer && (
        <Button
          size="compact-xs"
          variant="light"
          loading={busy === "share"}
          onClick={() =>
            void act("share", "Start sharing", async () => {
              const r = await api.startShare(props.run);
              if (!r?.join_url) return;
              // Reported separately, and never as a failure of the share: the
              // share is live by this point, and a refused clipboard write
              // (no permission, a non-secure origin) that surfaced as
              // "Start sharing failed" would send the user to undo something
              // that worked.
              try {
                await navigator.clipboard.writeText(r.join_url);
                notifyDone("Sharing — join link copied to the clipboard");
              } catch (e) {
                notifyError("Copy the join link", e);
              }
            })
          }
        >
          Share
        </Button>
      )}
      {props.peer && (
        <Button
          size="compact-xs"
          color="red"
          variant="light"
          loading={busy === "stop-share"}
          onClick={() => void act("stop-share", "Stop sharing", () => api.stopShare(props.peer!.id))}
        >
          Stop sharing
        </Button>
      )}
      {props.running && props.web.length === 0 && (
        <Button
          size="compact-xs"
          variant="light"
          loading={busy === "web-share"}
          onClick={() => void act("web-share", "Share to the web", () => api.startShare(props.run, { web: true }))}
        >
          Share to web
        </Button>
      )}
    </>
  );
}

/** The live peer share: joiners, the link and command, auto-accept, transports. */
export function PeerShareStrip(props: {
  share: ShareInfo;
  onChanged: () => void;
}) {
  const { act } = useShareAction(props.onChanged);
  const { flash, copy } = useCopyFlash();
  const share = props.share;
  return (
    <Group gap={6} px={12} pb={6} wrap="wrap" className="share-strip">
      <span className="dot running" style={{ animation: "none" }} />
      <Text size="xs">Sharing</Text>
      {share.joiners > 0 && (
        <Text size="xs" c="dimmed">
          · <b>{share.joiners}</b> connected
        </Text>
      )}
      {share.join_url && (
        <Button size="compact-xs" variant="default" onClick={() => copy(share.join_url!, "join")}>
          {flash === "join" ? "Link copied!" : "Copy link"}
        </Button>
      )}
      {share.ticket && (
        <Button
          size="compact-xs"
          variant="default"
          onClick={() => copy(`veld join ${share.ticket}`, "join-cmd")}
        >
          {flash === "join-cmd" ? "Copied" : "Copy command"}
        </Button>
      )}
      <Checkbox
        size="xs"
        label="auto-accept"
        checked={share.approve === "auto"}
        onChange={(e) =>
          void act("mode", "Change the approval mode", () =>
            api.setShareMode(share.id, e.currentTarget.checked ? "auto" : "manual"),
          )
        }
      />
      <ConnBadges share={share} />
    </Group>
  );
}

/** One public web share: its URLs (password-bearing where there is one), transports. */
export function WebShareStrip(props: {
  share: ShareInfo;
  onChanged: () => void;
}) {
  const { act } = useShareAction(props.onChanged);
  const { flash, copy } = useCopyFlash();
  const w = props.share;
  return (
    <Group gap={6} px={12} pb={6} wrap="wrap" className="share-strip">
      <span className="dot running" style={{ animation: "none" }} />
      <Text size="xs">Public web</Text>
      {w.public_urls.map((u) => {
        const withPassword = !!w.web_password && u.access !== "link";
        const link = withPassword
          ? `${u.public_url}/#veld-key=${encodeURIComponent(w.web_password!)}`
          : u.public_url;
        const tag = `web-${w.id}-${u.node}`;
        return (
          <Button key={u.node} size="compact-xs" variant="default" onClick={() => copy(link, tag)}>
            {flash === tag ? "Copied" : `${u.node} ${withPassword ? "link (with password)" : "URL"}`}
          </Button>
        );
      })}
      {w.web_password && (
        <Button
          size="compact-xs"
          variant="default"
          onClick={() => copy(w.web_password!, `pw-${w.id}`)}
        >
          {flash === `pw-${w.id}` ? "Copied" : "Copy password"}
        </Button>
      )}
      <Button
        size="compact-xs"
        color="red"
        variant="light"
        onClick={() => void act("stop-share", "Stop the web share", () => api.stopShare(w.id))}
      >
        Stop web
      </Button>
      <ConnBadges share={w} />
    </Group>
  );
}

/**
 * The whole sharing state of one run, as a panel.
 *
 * IDE mode's top-level Sharing surface: the controls, the live peer share and any
 * public web share, for the selected worktree's run. Deliberately *not* the join
 * requests — those are time-sensitive and belong where they are visible without
 * opening anything (a banner), and they can arrive for a share whose worktree is
 * not the selected one.
 */
export function RunSharePanel(props: {
  /** The selected worktree's run, or null when there is nothing shareable. */
  run: RunRef | null;
  runId: string | null;
  running: boolean;
  shares: ShareInfo[];
  /** The share list could not be read, so `shares` says nothing about reality. */
  unknown?: boolean;
  /**
   * Why there is nothing to share — the host knows, this doesn't. Distinct from
   * the diagnostics panes' hint on purpose: "start the run and its logs appear
   * here" is the wrong sentence under a Sharing control.
   */
  emptyHint: string;
  onChanged: () => void;
}) {
  if (!props.run || !props.runId) {
    return (
      <Text size="xs" c="dimmed" p={10}>
        {props.emptyHint}
      </Text>
    );
  }
  const { peer, web } = sharesForRun(props.shares, props.runId);
  const idle = !peer && web.length === 0;
  return (
    <div className="share-panel">
      <Group gap={6} px={12} pt={10} pb={idle ? 10 : 6} wrap="wrap">
        <ShareControls
          run={props.run}
          running={props.running}
          peer={peer}
          web={web}
          onChanged={props.onChanged}
        />
        {idle && !props.running && (
          <Text size="xs" c="dimmed">
            {props.emptyHint}
          </Text>
        )}
        {idle && props.running && (
          <Text size="xs" c="dimmed">
            {props.unknown
              ? "Can't read the share list right now — retrying."
              : "This run has nothing shared yet."}
          </Text>
        )}
      </Group>
      {peer && (
        <PeerShareStrip share={peer} onChanged={props.onChanged} />
      )}
      {web.map((w) => (
        <WebShareStrip key={w.id} share={w} onChanged={props.onChanged} />
      ))}
    </div>
  );
}

/**
 * One pending join request: someone is waiting for an answer.
 *
 * `runLabel` names which run they are asking about — a request carries only its
 * `share_id`, and a surface that shows requests for every share (IDE mode's
 * banner does, deliberately: a request against a worktree you are not looking at
 * must not be invisible) would otherwise ask the user to approve an unnamed thing.
 */
export function JoinRequestRow(props: {
  pending: PendingInfo;
  runLabel?: string | null;
  onChanged: () => void;
}) {
  const { busy, act } = useShareAction(props.onChanged);
  const p = props.pending;
  return (
    <Group gap="xs" className="share-row pending" p={8} wrap="wrap">
      <Badge size="xs" color="yellow" variant="light">
        join request
      </Badge>
      <Text size="xs">
        <b>{p.label || "(no label)"}</b> wants to join
        {props.runLabel ? (
          <>
            {" "}
            <b>{props.runLabel}</b>
          </>
        ) : null}
      </Text>
      <Text size="xs" c="dimmed" ff="monospace">
        {p.share_id} · {p.node_id.slice(0, 10)}
      </Text>
      <div style={{ flex: 1 }} />
      <Button
        size="compact-xs"
        variant="light"
        loading={busy === "approve"}
        onClick={() => void act("approve", "Approve the join request", () => api.approveJoin(p.id))}
      >
        Approve
      </Button>
      <Button
        size="compact-xs"
        color="red"
        variant="light"
        loading={busy === "deny"}
        onClick={() => void act("deny", "Deny the join request", () => api.denyJoin(p.id))}
      >
        Deny
      </Button>
    </Group>
  );
}
