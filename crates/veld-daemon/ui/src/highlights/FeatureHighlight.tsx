import { Button, CloseButton, Group, Popover, Stack, Text } from "@mantine/core";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";

import { api } from "../api";
import { mergeStates, type PromotionState, utcDay } from "../promotions/model";
import { HIGHLIGHTS, type HighlightSlug } from "./content";
import { highlightEligible, highlightId } from "./model";

interface HighlightStore {
  states: Record<string, PromotionState> | null;
  firstUse: string | null;
  /** The one highlight this page load has spent its turn on, or `null`. */
  shown: HighlightSlug | null;
  claim: (slug: HighlightSlug) => void;
  settle: (slug: HighlightSlug, state: PromotionState) => void;
}

const Store = createContext<HighlightStore | null>(null);

/**
 * Seen-state for every highlight on the page, and the one-per-load budget.
 *
 * Reads the promotions state map itself rather than borrowing `usePromotions`'s,
 * so the two features stay independent: the request is one small POST, the
 * server-side merge is monotone, and neither side has to know the other exists.
 *
 * **One highlight per page load.** The first one that becomes eligible while its
 * anchor is on screen claims the turn; every other waits for a later load. Several
 * bubbles at once — or one after another as somebody clicks through the app — is
 * how a hint system turns into something people close without reading.
 */
export function HighlightProvider(props: { children: React.ReactNode }) {
  const [states, setStates] = useState<Record<string, PromotionState> | null>(null);
  const [firstUse, setFirstUse] = useState<string | null>(null);
  const [shown, setShown] = useState<HighlightSlug | null>(null);

  useEffect(() => {
    let cancelled = false;
    void api
      .promotionState()
      .then((res) => {
        if (cancelled) return;
        setStates(res.states);
        setFirstUse(res.first_use);
      })
      // Silence, as for the cards: a hint is never worth an error, and leaving the
      // map null is what keeps every bubble closed rather than guessing.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const claim = useCallback((slug: HighlightSlug) => {
    setShown((current) => current ?? slug);
  }, []);

  const settle = useCallback((slug: HighlightSlug, state: PromotionState) => {
    const id = highlightId(slug);
    // Optimistic, so the bubble stays shut even if the write fails — a hint shown
    // once more on the next load is a far smaller cost than one that will not close.
    setStates((current) => mergeStates(current ?? {}, [id], state));
    void api
      .markPromotions([id], state)
      .then((res) => setStates(res.states))
      .catch(() => {});
  }, []);

  return (
    <Store.Provider value={{ states, firstUse, shown, claim, settle }}>
      {props.children}
    </Store.Provider>
  );
}

/**
 * Pin a highlight's bubble to one control.
 *
 * Wraps the control rather than being placed beside it, so the bubble points at
 * exactly what it is about, and a control that is not rendered cannot show one.
 * Outside a `HighlightProvider` (a test, a surface that opted out) it renders the
 * child alone.
 *
 * Closed only by **Got it**, the ✕, or Esc — not by a click elsewhere, which in a
 * dialog is usually somebody already doing the thing the bubble is about, and a
 * hint that vanished at the first click would be recorded as seen without being
 * read. `Got it` stores `read`, the others `dismissed`; either ends it.
 */
export function FeatureHighlight(props: {
  slug: HighlightSlug;
  position?: "top" | "bottom" | "left" | "right";
  children: React.ReactElement;
}) {
  const store = useContext(Store);
  const h = HIGHLIGHTS[props.slug];
  const eligible =
    store !== null &&
    highlightEligible(h, store.states, store.firstUse, utcDay(new Date().toISOString())) &&
    (store.shown === null || store.shown === props.slug);

  useEffect(() => {
    if (eligible) store?.claim(props.slug);
  }, [eligible, store, props.slug]);

  // One answer per bubble. Mantine's `Popover` calls `onClose` itself when a
  // button inside it closes it, so without this *Got it* also wrote `dismissed` —
  // harmless against the monotone merge, but a second request for nothing.
  const answered = useRef(false);

  if (store === null) return props.children;
  const opened = eligible && store.shown === props.slug;
  const close = (state: PromotionState) => {
    if (answered.current) return;
    answered.current = true;
    store.settle(props.slug, state);
  };

  return (
    <Popover
      opened={opened}
      position={props.position ?? "top"}
      withArrow
      shadow="md"
      radius="md"
      width={260}
      closeOnClickOutside={false}
      onClose={() => close("dismissed")}
      trapFocus={false}
      returnFocus={false}
    >
      <Popover.Target>{props.children}</Popover.Target>
      <Popover.Dropdown>
        <Stack gap={6} role="status" aria-live="polite">
          <Group justify="space-between" wrap="nowrap" align="flex-start">
            <Text size="sm" fw={600}>
              {h.title}
            </Text>
            <CloseButton
              size="sm"
              aria-label="Dismiss this tip"
              onClick={() => close("dismissed")}
            />
          </Group>
          <Text size="xs" c="dimmed">
            {h.body}
          </Text>
          <Group justify="flex-end">
            <Button size="compact-xs" variant="light" onClick={() => close("read")}>
              Got it
            </Button>
          </Group>
        </Stack>
      </Popover.Dropdown>
    </Popover>
  );
}
