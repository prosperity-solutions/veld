/**
 * The what's-new panel — the same sections, stacked in a dialog.
 *
 * **Its two exits are not the same act**, and that is the reason this component
 * takes two callbacks rather than one `onClose`:
 *
 * - *Got it* means **read**. It clears the unread indicator.
 * - Esc, the close button and the overlay mean **dismissed**: stop putting this
 *   in front of me. The card never prompts again, but it stays unread in the ⋯
 *   menu, so clearing a modal in the middle of something does not lose it.
 *
 * Opened from the menu instead, there is nothing to acknowledge — the user came
 * here on purpose, so both exits mean read.
 */

import { Button, ScrollArea, Stack } from "@mantine/core";

import { Modal } from "../components/dialogs";
import { Wordmark } from "./Brand";
import type { Promotion } from "./model";
import { PromoSection } from "./Section";

export function WhatsNewDialog(props: {
  promotions: Promotion[];
  /** Whether this opened itself, rather than being asked for. */
  automatic: boolean;
  onRead: () => void;
  onDismiss: () => void;
}) {
  return (
    <Modal
      title="What's new in Veld"
      // Esc / close button / overlay. Reading is the button's job alone.
      onClose={props.automatic ? props.onDismiss : props.onRead}
    >
      <Stack gap="lg">
        {/* The worst case is somebody who has not opened Veld in months and
            meets a year of cards at once. `Autosize` so two cards still render
            as two cards rather than in a tall fixed box, and `type="always"` so
            the bar is visible from the first frame — a scroll region whose only
            hint is an overflowing card gets read as the end of the list. */}
        <ScrollArea.Autosize mah="min(58vh, 520px)" type="always" offsetScrollbars={true}>
          <div className="promo-stack">
            {props.promotions.map((p) => (
              <PromoSection key={p.id} section={p} day={p.since} />
            ))}
          </div>
        </ScrollArea.Autosize>
        <div className="promo-dialog-foot">
          <Wordmark height={18} />
          {/* One label for both entrances: this button means *read* either way,
              and a control that changes its name by how the dialog opened makes
              the reader wonder whether it also changed what it does. */}
          <Button onClick={props.onRead}>Got it!</Button>
        </div>
      </Stack>
    </Modal>
  );
}
