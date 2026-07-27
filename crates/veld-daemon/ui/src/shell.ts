// The Electron shell loads /ide?shell=electron: the top bar then doubles as
// the frameless window's native title bar (drag region, traffic-light inset).
export const isElectron =
  new URLSearchParams(window.location.search).get("shell") === "electron";

export const topbarClass = `topbar${isElectron ? " electron" : ""}`;
