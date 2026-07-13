import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { positionRadialButtons } from "./toolbar";
import { deps } from "../shared/registry";

export function updateListeningModule(): void {
  if (refs.fab) {
    refs.fab.classList.toggle(PREFIX + "fab-pulse", getState().agentListening);
  }
  // Recompute radial layout since listening dot visibility changed
  positionRadialButtons();
  // The "Currently running" lane derives from listening state — keep an open
  // panel in sync when the agent session starts or stops.
  if (getState().panelOpen) deps().renderPanel();
}

export function sendAllGood(): void {
  api("POST", "/session/end")
    .then(function () {
      toast("Done — feedback session ended.");
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
    })
    .catch(function (err: Error) {
      toast("Failed: " + err.message, true);
    });
}
