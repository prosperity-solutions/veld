import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";

export function updateListeningModule(): void {
  if (refs.listeningModule) {
    refs.listeningModule.style.display = getState().agentListening ? "flex" : "none";
  }
  if (refs.fab) {
    refs.fab.classList.toggle(PREFIX + "fab-pulse", getState().agentListening);
  }
}

export function sendAllGood(): void {
  api("POST", "/session/end")
    .then(function () {
      toast("All Good signal sent!");
      dispatch({ type: "SET_LISTENING", listening: false });
      updateListeningModule();
    })
    .catch(function (err: Error) {
      toast("Failed: " + err.message, true);
    });
}
