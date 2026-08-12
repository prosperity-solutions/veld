/**
 * The what's-new panel: a list you read and close.
 *
 * **One presentation, two entrances**, and that is a decision worth defending
 * because the alternative was built and rejected. A stepped flow — one
 * orientation card per page, then the news — read badly for a reason that is
 * obvious in hindsight: "what's new" is a genre the reader already knows, and it
 * is a *list*, the same one every changelog and release-note surface shows. A
 * stepper is a different genre (a first-run wizard, which teaches you before you
 * start), so using it here made the popup and the reopened panel look like two
 * different features, and its page grouping — one card, then one card, then two
 * together — was arbitrary from outside. The reader had to learn the surface
 * before reading the content.
 *
 * So the thing that pops up *is* the thing the ⋯ menu reopens. What differs is
 * only scope and what closing means:
 *
 * |                | Auto-open                | ⋯ → What's new…     |
 * |----------------|--------------------------|---------------------|
 * | shows          | what is outstanding      | everything          |
 * | filter         | no                       | yes, with a project |
 * | Esc / ✕ / veil | dismissed, still counted | read                |
 * | *Got it!*      | read                     | read                |
 *
 * **Those two exits are not the same act**, which is why this takes two callbacks
 * rather than one `onClose`:
 *
 * - *Got it* means **read**. It clears the unread indicator.
 * - Esc, the close button and the overlay mean **dismissed**: stop putting this in
 *   front of me. The card never prompts again, but it stays unread in the ⋯ menu,
 *   so clearing a modal in the middle of something does not lose it.
 *
 * Opened from the menu there is nothing to acknowledge — the reader came here on
 * purpose — so both exits mean read.
 *
 * The title says "What's new" and **not** "What's new in Veld", and the wordmark
 * is not in the footer. Both were right while every card was Veld's; with a
 * project's cards in the same list they would be Veld putting its name over text a
 * teammate wrote. Attribution belongs on each card, and it is there.
 *
 * **The card list is the scroll region**, bounded to `min(58vh, 560px)`, with the
 * filter above it and the footer below it — the same shape `NewWorktreeDialog`
 * uses, and worth copying rather than reinventing, because three other shapes were
 * tried here first. A Mantine `ScrollArea` inside an unbounded modal body gave two
 * scrollbars side by side (the body scrolls too). Making the body a fixed-height
 * flex column fixed that and clipped the filter row off the *top*. `position:
 * sticky` on the footer fixed *that* and left the button floating over the cards
 * with the body's scrollbar pinned to the dialog's outer edge. Bounding the list is
 * the version with one scrollbar, inset, and nothing overlapping anything.
 */

import { Button, SegmentedControl, Stack, Text } from "@mantine/core";
import { useState } from "react";

import { Modal } from "../components/dialogs";
import { type Card, type SourceFilter, filterOptions, historyOf } from "./model";
import { PromoSection } from "./Section";

export function WhatsNewDialog(props: {
  cards: Card[];
  /** Whether this opened itself, rather than being asked for. */
  automatic: boolean;
  /**
   * The selected project's name, when there is one — **even if it has no news.**
   *
   * That is the point of taking it separately from the cards: a project with
   * nothing to say still gets a tab, disabled, so "this project has told you
   * nothing" is a visible answer. Omitting the tab reads as the feature not
   * existing.
   */
  projectName: string | null;
  onRead: () => void;
  onDismiss: () => void;
}) {
  const [filter, setFilter] = useState<SourceFilter>("all");
  // `null` when there is nothing worth filtering between, including the whole
  // interrupting entrance — `filterOptions` owns that rule (and is tested on it,
  // which nothing here could be).
  const tabs = filterOptions(props.cards, props.projectName, props.automatic);
  const shown = historyOf(props.cards, tabs ? filter : "all");
  return (
    <Modal
      title="What's new"
      size={720}
      // Esc / close button / overlay. Reading is the button's job alone.
      onClose={props.automatic ? props.onDismiss : props.onRead}
    >
      <Stack gap="md">
        {tabs && (
          <SegmentedControl
            size="xs"
            fullWidth
            value={filter}
            onChange={(v) => setFilter(v as SourceFilter)}
            data={tabs}
          />
        )}
        {shown.length === 0 ? (
          <Text size="sm" c="dimmed">
            {props.projectName
              ? `${props.projectName} hasn't told its team anything yet. A project declares news in its veld.json, and it shows up here.`
              : "Nothing yet."}
          </Text>
        ) : (
          // One flat list, newest first, and no headings over it. Every card is a
          // change that landed on a day, so the date is the only grouping the
          // reader needs — and a taxonomy above a list of three is a label to skip.
          //
          // The list is the scroller, not the modal body: bounded height here is
          // what leaves exactly one scrollbar, inset from the dialog's edge, with
          // the filter above it and the footer below it both staying put.
          <div className="promo-dialog-scroll">
            <div className="promo-stack">
              {shown.map((c) => (
                <PromoSection key={c.id} section={c} day={c.since} source={c.source} />
              ))}
            </div>
          </div>
        )}
        <div className="promo-dialog-foot">
          {/* One label for both entrances: this button means *read* either way, and
              a control that changes its name by how the dialog opened makes the
              reader wonder whether it also changed what it does.

              Note what closing marks: everything the panel was *given*, not the
              filtered subset. A filter is a way of looking at the list, not a way
              of leaving part of it unread — and a reader who filtered to one
              project and closed would otherwise keep a badge they cannot
              explain. */}
          <Button onClick={props.onRead}>Got it!</Button>
        </div>
      </Stack>
    </Modal>
  );
}
