import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { positionRadialButtons } from "./toolbar";
import { clearSession } from "./persist";

export function updateListeningModule(): void {
  if (refs.fab) {
    refs.fab.classList.toggle(PREFIX + "fab-pulse", getState().agentListening);
  }
  // Recompute radial layout since listening dot visibility changed
  positionRadialButtons();
}

export function sendAllGood(): void {
  api("POST", "/session/end")
    .then(function () {
      toast("Done — feedback session ended.");
      // The reviewer deliberately clicked Done — drop their unsent tab-local
      // drafts/UI state so a later reload doesn't resurrect a composer from a
      // session they finished. Deliberately NOT cleared on an agent-side
      // session end (the session_ended / agent_stopped events in polling.ts):
      // an agent stopping is not the reviewer abandoning their half-typed work,
      // so that work is preserved across the agent's restart.
      clearSession();
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
    })
    .catch(function (err: Error) {
      toast("Failed: " + err.message, true);
    });
}
