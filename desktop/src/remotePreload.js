const { contextBridge, ipcRenderer } = require("electron");

// This bridge is loaded only by remoteChooser.html, never by a remote window.
// There is no generic channel selector, event object, or existing Desktop bridge.
contextBridge.exposeInMainWorld("veldRemote", Object.freeze({
  list: () => ipcRenderer.invoke("veld:remote:list"),
  discover: () => ipcRenderer.invoke("veld:remote:discover"),
  pair: (invitation, label) => ipcRenderer.invoke("veld:remote:pair", invitation, label),
  open: (id) => ipcRenderer.invoke("veld:remote:open", id),
  forget: (id) => ipcRenderer.invoke("veld:remote:forget", id),
}));
