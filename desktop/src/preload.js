// Minimal bridge: lets the web UI know it runs inside the desktop shell.
// No IPC surface yet — webview/session APIs arrive with later increments.
const { contextBridge } = require("electron");

contextBridge.exposeInMainWorld("veldDesktop", {
  shell: "electron",
  version: process.versions.electron,
});
