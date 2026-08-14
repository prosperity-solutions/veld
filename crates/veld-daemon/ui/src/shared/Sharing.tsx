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

import { Badge, Button, Checkbox, Group, Stack, Text, Tooltip } from "@mantine/core";
import {
  IconAlertCircle,
  IconChevronDown,
  IconChevronRight,
  IconCircleCheck,
  IconEye,
  IconEyeOff,
} from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { api, type PendingInfo, type RunRef, type ShareInfo } from "../api";
import { useCopyFlash } from "./copy";
import { QrCode } from "./QrCode";
import { copyLinkWithQr, copyQrImage } from "./qrClipboard";
import { notifyDone, notifyError } from "./notify";
import { formatRemaining, useCaffeinate } from "./useCaffeinate";

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
 * The two modes are independent, so each stays available even while the other is
 * live: "share with another Veld user" is offered whenever there is no peer
 * share, and "share publicly" whenever there is no web share. Starting a peer
 * share copies the join link straight to the clipboard — that is the entire
 * point of starting one — and confirms it with a toast, because the button that
 * was clicked is replaced by "Stop sharing" in the same breath and so cannot
 * carry the confirmation itself.
 *
 * Stop buttons are deliberately the loudest thing here (`filled` red): the
 * first test's user read "share privately" at the top and missed the stop
 * control a beat, so a live share's exit must not be one more quiet button.
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
          title="A direct encrypted tunnel to someone else's Veld — no gateway, no public URL, and they need Veld installed"
          loading={busy === "share"}
          onClick={() =>
            void act("share", "Start sharing", async () => {
              const r = await api.startShare(props.run);
              if (!r?.join_url) return;
              // Never reported as a failure of the *share*: it is live by this
              // point, and "Start sharing failed" would send the user to undo
              // something that worked. Not reported as an error at all, either —
              // WebKit only allows a clipboard write in the same task as the
              // gesture, and this one follows an awaited round-trip, so in the
              // browser build the refusal is the norm rather than a fault. The
              // strip that just appeared carries a Copy link button; say that.
              try {
                await navigator.clipboard.writeText(r.join_url);
                notifyDone("Sharing — join link copied to the clipboard");
              } catch {
                notifyDone("Sharing — use Copy link to get the invite");
              }
            })
          }
        >
          Share with another Veld user
        </Button>
      )}
      {props.peer && (
        <Button
          size="compact-xs"
          color="red"
          variant="filled"
          loading={busy === "stop-share"}
          onClick={() => void act("stop-share", "Stop sharing", () => api.stopShare(props.peer!.id))}
        >
          {MODE_COPY.peer.stop}
        </Button>
      )}
      {props.running && props.web.length === 0 && (
        <Button
          size="compact-xs"
          variant="light"
          loading={busy === "web-share"}
          title="A public URL anyone can open in a browser, no Veld needed — routed through the veld gateway"
          onClick={() => void act("web-share", "Share to the web", () => api.startShare(props.run, { web: true }))}
        >
          Share publicly
        </Button>
      )}
    </>
  );
}

/** The structured copy for one sharing mode — kept as data so the card is
 *  scannable (a short use case, a short pros list, one requirement) instead of
 *  one long paragraph. The same mode names are the start buttons' copy, and the
 *  stop buttons mirror them. */
const MODE_COPY = {
  peer: {
    title: "Share with another Veld user",
    bestFor: "Real access — databases, internal tools",
    pros: [
      "More services are eligible",
      "Safer: direct end-to-end encrypted tunnel",
      "No public URL",
    ],
    requires: "They need Veld installed",
    warning: true,
    cta: "Start sharing",
    stop: "Stop sharing with Veld user",
  },
  web: {
    title: "Share publicly",
    bestFor: "People without Veld — or your phone, via QR",
    pros: [
      "No Veld needed on the other end",
      "Open it on any device from a link or QR code",
      "Perfect for mobile testing on your phone",
    ],
    requires: undefined,
    warning: false,
    cta: "Start sharing",
    stop: "Stop public share",
  },
} as const;

type ShareModeKind = keyof typeof MODE_COPY;

/**
 * One big card explaining a sharing mode — and the button that starts it.
 *
 * The whole card is the button (a nested button would be invalid HTML), with a
 * short scannable layout: a one-line use case, a pros list, and one requirement.
 * That replaces the earlier terse-but-paragraph form, which a first user read
 * as a label rather than an action.
 */
export function ShareStartCard(props: {
  kind: ShareModeKind;
  /** The live run to share. */
  run: RunRef;
  onChanged: () => void;
}) {
  const { busy, act } = useShareAction(props.onChanged);
  const copy = MODE_COPY[props.kind];
  const starting = busy === "share" || busy === "web-share";
  const start = () => {
    if (props.kind === "peer") {
      void act("share", "Start sharing", async () => {
        const r = await api.startShare(props.run);
        if (!r?.join_url) return;
        try {
          await navigator.clipboard.writeText(r.join_url);
          notifyDone("Sharing — join link copied to the clipboard");
        } catch {
          notifyDone("Sharing — use Copy link to get the invite");
        }
      });
    } else {
      void act("web-share", "Share to the web", () =>
        api.startShare(props.run, { web: true }),
      );
    }
  };
  return (
    <button
      type="button"
      className="share-mode-card"
      onClick={start}
      disabled={starting}
    >
      <Text size="sm" fw={700}>
        {copy.title}
      </Text>
      <Text size="xs" c="dimmed" className="share-mode-bestfor">
        <b>Main use case:</b> {copy.bestFor}
      </Text>
      <div className="share-mode-list">
        {copy.pros.map((p) => (
          <Text size="xs" key={p} className="share-mode-li">
            <IconCircleCheck size={12} /> {p}
          </Text>
        ))}
      </div>
      {copy.requires && (
        <Text size="xs" className={`share-mode-note${copy.warning ? " warn" : ""}`}>
          <IconAlertCircle size={12} /> {copy.requires}
        </Text>
      )}
      <span className={`share-mode-cta${starting ? " starting" : ""}`}>
        {starting ? "Starting…" : copy.cta}
      </span>
    </button>
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
      <Text size="xs" fw={600}>
        Private (peer to peer)
      </Text>
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

/**
 * The link for one public URL of a web share: password-bearing where there is one.
 *
 * One function because the copy button and the QR must encode the *same* string —
 * a QR that dropped the password fragment would send someone to a login page they
 * cannot get past on the device they just scanned with.
 */
function webShareLink(
  share: ShareInfo,
  url: { public_url: string; access?: string | null },
): string {
  const withPassword = !!share.web_password && url.access !== "link";
  return withPassword
    ? `${url.public_url}/#veld-key=${encodeURIComponent(share.web_password!)}`
    : url.public_url;
}

/**
 * One public web share: a header with what applies to the whole share, then a row
 * per service.
 *
 * Rows rather than a wrapping bag of buttons — a share of three services produced six
 * buttons whose labels were the only thing telling them apart.
 *
 * **The URL itself is not rendered.** It is a 26-character random subdomain: nobody
 * reads it, nobody retypes it, and truncated to fit a row it says nothing at all. The
 * service name identifies the row, the QR is the thing you point a phone at, and the
 * buttons are how the URL leaves this panel. It is still in every button's payload and
 * in the QR — this is a display decision, not a data one.
 *
 * **Each code is blurred until its row is hovered.** Two problems, one mechanism: with
 * three services on screen it was genuinely unclear which code your phone was pointed
 * at, and a code that carries the share password is a credential sitting on screen.
 * Hover reveals exactly one, and *Show all* reveals every one at once — which is also
 * the reveal that works without a pointer, so the blur is never a hover-only trap.
 */
export function WebShareStrip(props: {
  share: ShareInfo;
  onChanged: () => void;
  /**
   * Start collapsed behind a one-line summary, expandable.
   *
   * For runs mode, where the premise is *seeing every run*: three services with a code
   * each is taller than the card it hangs under, and pushed the node list off the
   * screen. In the sharing modal — a surface opened to do exactly this — it stays
   * expanded, because collapsing the only thing the dialog contains is furniture.
   */
  collapsible?: boolean;
}) {
  const { act } = useShareAction(props.onChanged);
  const { flash, copy } = useCopyFlash();
  const w = props.share;
  const [expanded, setExpanded] = useState(!props.collapsible);
  /**
   * Reveal every code at once.
   *
   * One switch rather than a per-code pin: pinning meant tracking which of three codes
   * was deliberately visible, for the sake of a case — "I want *this* one unblurred
   * while the mouse is elsewhere" — that Show all already covers.
   */
  const [revealAll, setRevealAll] = useState(false);

  /** Copy the link and a picture of it; report which flavours actually landed. */
  const copyBoth = async (link: string) => {
    try {
      const result = await copyLinkWithQr(link);
      notifyDone(
        result === "both"
          ? "Link and QR code copied — paste into a chat"
          : "Link copied (this browser would not take the image)",
      );
    } catch (e) {
      notifyError("Copy the link and QR code", e);
    }
  };

  /** Copy only the picture — the way to send the code *after* the link. */
  const copyImage = async (link: string) => {
    try {
      await copyQrImage(link);
      notifyDone("QR code copied as an image");
    } catch (e) {
      notifyError("Copy the QR code", e);
    }
  };

  return (
    <Stack gap={8} px={12} pb={8} className="share-strip">
      <Group gap={6} wrap="wrap">
        <span className="dot running" style={{ animation: "none" }} />
        {props.collapsible ? (
          <Button
            size="compact-xs"
            variant="subtle"
            px={4}
            leftSection={
              expanded ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />
            }
            onClick={() => setExpanded((v) => !v)}
          >
            Public web · {w.public_urls.length}{" "}
            {w.public_urls.length === 1 ? "service" : "services"}
          </Button>
        ) : (
          <Text size="xs" fw={600}>
            Public web
          </Text>
        )}
        {w.web_password && (
          <Button
            size="compact-xs"
            variant="default"
            onClick={() => copy(w.web_password!, `pw-${w.id}`)}
          >
            {flash === `pw-${w.id}` ? "Copied" : "Copy password"}
          </Button>
        )}
        {expanded && (
          <>
            <Button
              size="compact-xs"
              variant={revealAll ? "light" : "default"}
              leftSection={
                revealAll ? <IconEyeOff size={13} /> : <IconEye size={13} />
              }
              onClick={() => setRevealAll((v) => !v)}
            >
              {revealAll ? "Hide all" : "Show all"}
            </Button>
            {/* The hint belongs beside the control it is an alternative to. Without it
                the blur reads as a rendering fault rather than as something with an
                obvious way through it. */}
            <Text size="xs" c="dimmed">
              or hover a code to reveal it
            </Text>
          </>
        )}
        <div style={{ flex: 1 }} />
        <Button
          size="compact-xs"
          color="red"
          variant="filled"
          onClick={() => void act("stop-share", "Stop the web share", () => api.stopShare(w.id))}
        >
          {MODE_COPY.web.stop}
        </Button>
      </Group>
      {/* Scrolls past four services rather than growing without limit: a run can share
          as many services as the config declares, and this panel lives inside a modal
          and inside a card, neither of which can absorb ten rows. */}
      {expanded && (
      <Stack
        gap={8}
        style={{ maxHeight: "min(46vh, 420px)", overflowY: "auto" }}
      >
      {w.public_urls.map((u) => {
        const withPassword = !!w.web_password && u.access !== "link";
        const link = webShareLink(w, u);
        const tag = `web-${w.id}-${u.node}`;
        return (
          <Group key={u.node} gap={12} p={10} wrap="nowrap" align="start" className="share-row">
            {/* The QR's own white card sits inside the row's padding rather than
                against its rounded corner, where the square white corner of the code
                poked out past the radius. */}
            {/* `sensitive` is what applies the blur, inside `QrCode` — this link
                carries the share password, and the shield is not something a call site
                can forget to wrap. */}
            <QrCode
              value={link}
              label={`QR code for the ${u.node} public URL`}
              sensitive
              revealed={revealAll}
            />
            {/* `minWidth: 0` is what lets the button row wrap instead of pushing the QR
                out of a narrow panel — a flex child's default min-width is its
                content. */}
            <Stack gap={6} style={{ flex: 1, minWidth: 0 }}>
              <Text size="sm" fw={700}>
                {u.node}
                {withPassword && (
                  <Text span size="xs" c="dimmed" fw={400}>
                    {" "}
                    · link includes the password
                  </Text>
                )}
              </Text>
              <Group gap={6} wrap="wrap">
                <Button size="compact-xs" variant="default" onClick={() => copy(link, tag)}>
                  {flash === tag ? "Copied" : "Copy link"}
                </Button>
                <Button
                  size="compact-xs"
                  variant="default"
                  onClick={() => void copyBoth(link)}
                  title="Puts the link and a picture of the QR code on the clipboard — a chat app will paste one of them"
                >
                  Copy link + QR
                </Button>
                <Button
                  size="compact-xs"
                  variant="default"
                  onClick={() => void copyImage(link)}
                  title="Just the picture — for pasting the code after the link"
                >
                  Copy QR
                </Button>
              </Group>
            </Stack>
          </Group>
        );
      })}
      </Stack>
      )}
      {expanded && (
        <Text size="xs" c="dimmed">
          Scan a revealed code with a phone camera to open that service.
          {w.web_password ? " The codes carry the password — treat them like one." : ""}
        </Text>
      )}
      <Group gap={6} wrap="wrap">
        <ConnBadges share={w} />
      </Group>
    </Stack>
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
   * Live shares belonging to *another* run of the same worktree.
   *
   * They exist for two reasons the panel cannot fix by itself: a worktree can hold
   * more than one environment (the run selector binds this panel to one of them),
   * and a crashed run's shares outlive it until the GC pass releases them. Either way a public URL may
   * still be serving, so the panel shows them rather than offering to start a
   * second share of the same thing — the same problem runs mode solves with
   * `unattachedShareIds`.
   */
  otherRuns?: ShareInfo[];
  /**
   * Why there is nothing to share — the host knows, this doesn't. Distinct from
   * the diagnostics panes' hint on purpose: "start the run and its logs appear
   * here" is the wrong sentence under a Sharing control.
   */
  emptyHint: string;
  /**
   * Collapse each web share behind a summary line.
   *
   * Set by runs mode, where this panel hangs on a card in a list of every run and a
   * code per service pushes the node list off the screen. The sharing modal leaves it
   * off: collapsing the only thing that dialog contains would be furniture.
   */
  collapsibleShares?: boolean;
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
  // The two-mode framing, only in the modal's idle state: a first user is
  // choosing *how* to share, so each approach gets a card that says what it is
  // for. Once something is live, compact controls take over and the strips carry
  // the detail.
  const startCards =
    idle && props.running && props.run ? (
      <>
        <Text size="xs" c="dimmed" px={12} pt={10}>
          Share this run two ways. Pick the one that fits who you are sharing with.
        </Text>
        <div className="share-mode-cards">
          <ShareStartCard kind="peer" run={props.run} onChanged={props.onChanged} />
          <ShareStartCard kind="web" run={props.run} onChanged={props.onChanged} />
        </div>
        {props.unknown && (
          <Text size="xs" c="dimmed" px={12} pb={8}>
            Can&apos;t read the share list right now — retrying.
          </Text>
        )}
      </>
    ) : null;

  return (
    <div className="share-panel">
      {startCards ?? (
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
        </Group>
      )}
      {peer && (
        <PeerShareStrip share={peer} onChanged={props.onChanged} />
      )}
      {web.map((w) => (
        <WebShareStrip
          key={w.id}
          share={w}
          collapsible={props.collapsibleShares}
          onChanged={props.onChanged}
        />
      ))}
      {(props.otherRuns ?? []).map((s) => (
        <OtherRunShare key={s.id} share={s} onChanged={props.onChanged} />
      ))}
      {/* Only in the modal, never on a runs-mode card. Not a layout preference:
          this line reads the machine's keep-awake state, and runs mode renders
          one of these panels *per environment* — so a card-level note would mean
          a poller per card for one machine-wide fact. Runs mode gets the same
          answer from the coffee cup in its top bar, which is exactly why that
          cluster stopped being IDE-only. */}
      {!props.collapsibleShares && !idle && <KeepAwakeNote />}
    </div>
  );
}

/**
 * What sharing is doing to this machine's sleep, said where the sharing is.
 *
 * The contextual half of telling somebody about a hold they did not ask for: the
 * coffee cup shows *that* the machine is awake, and this says *why* at the moment
 * they are looking at the thing causing it. Silent when there is nothing to
 * report — a line saying "this machine may sleep" under every share would be
 * noise on the common path, where the machine sleeping is what it always did.
 */
function KeepAwakeNote() {
  const { state } = useCaffeinate();
  if (!state) return null;

  // Both "may sleep" branches are gated on the machine **not** being held, the
  // same way the cup's are. `hold_failed` and `sharing_spent` describe the
  // automatic half and outlive it: a user who reacts to either by clicking a
  // duration themselves is now genuinely held awake, and a panel still saying
  // "it may sleep" is the pessimistic twin of the optimistic lie this module's
  // docs call worse than no status at all.
  const note = !state.active && state.hold_failed
    ? "Veld could not keep this machine awake — it may sleep and drop the share."
    : !state.active && state.sharing_spent
    ? "This machine may sleep now — its automatic keep-awake for this share is used up."
    : state.reason === "sharing" || state.reason === "both"
      ? typeof state.remaining_secs === "number"
        ? `This machine will stay awake for another ${formatRemaining(state.remaining_secs)}${
            // Whose deadline that is, when it is not the keep-awake setting's.
            // Said here as well as on the cup because this panel is where
            // somebody reads it as a promise about *the share*, and the number
            // being the share's own expiry is the thing that makes "for at most
            // 4 hours" look broken when it is not.
            //
            // **`"sharing"` only, never `"both"`** — and that exclusion is the
            // whole correctness of this line. `remaining_secs` is
            // `Reasons::expires_at()`, the *later* of the two deadlines
            // (`m.max(s)`), while `sharing_bound_by_share` describes only the
            // automatic one. So under `"both"` a manual 4h hold taken during a
            // 2h share shows the manual number, and attributing it to the share
            // would be a fresh instance of exactly the mis-attribution this
            // field was added to remove.
            state.reason === "sharing" && state.sharing_bound_by_share
              ? " — when the share itself expires"
              : ""
          }${state.covers_lid ? "." : ", unless you shut the lid."}`
        : "This machine is being kept awake."
      : null;
  if (!note) return null;

  return (
    <Text size="xs" c="dimmed" px={12} pb={8}>
      {note}
    </Text>
  );
}

/** A share of another of this worktree's runs — named, and stoppable. */
function OtherRunShare(props: { share: ShareInfo; onChanged: () => void }) {
  const { busy, act } = useShareAction(props.onChanged);
  const s = props.share;
  const web = s.public_urls.length > 0;
  return (
    <Group gap={6} px={12} pb={6} wrap="wrap" className="share-strip">
      <span className="dot partial" style={{ animation: "none" }} />
      <Text size="xs">
        {web ? "Public web" : "Sharing"} · run <b>{s.run || s.id}</b>
      </Text>
      <Text size="xs" c="dimmed">
        not the run shown here
      </Text>
      <div style={{ flex: 1 }} />
      <Button
        size="compact-xs"
        color="red"
        variant="light"
        loading={busy === "stop-other"}
        onClick={() => void act("stop-other", "Stop that share", () => api.stopShare(s.id))}
      >
        Stop
      </Button>
    </Group>
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
