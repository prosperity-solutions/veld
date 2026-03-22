import { S } from "./state";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";

export function updateListeningModule(): void {
  if (S.listeningModule) {
    S.listeningModule.style.display = S.agentListening ? "flex" : "none";
  }
  if (S.fab) {
    S.fab.classList.toggle(PREFIX + "fab-pulse", S.agentListening);
  }
}

export function sendAllGood(): void {
  api("POST", "/session/end")
    .then(function () {
      toast("All Good signal sent!");
      S.agentListening = false;
      updateListeningModule();
    })
    .catch(function (err: Error) {
      toast("Failed: " + err.message, true);
    });
}
