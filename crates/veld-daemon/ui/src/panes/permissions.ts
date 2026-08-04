/**
 * How permissions are worded in the UI.
 *
 * The policy itself lives in the desktop shell (`desktop/src/permissions.js`) —
 * this is only the vocabulary, kept out of the component so a label can be
 * changed without touching the pane, and so the browser build can import it
 * without pulling in anything Electron-shaped.
 *
 * The wording is what makes a prompt honest, so it is deliberately concrete:
 * *"use your camera"*, not *"camera"*. A prompt reading "example.com wants
 * camera" is a permission dialog written for the person who implemented it.
 */

import type { PermissionId } from "../api";
import type { PermissionSetting } from "./browserHost";

/** Sentence fragment, completing "<origin> wants to …". */
export const PERMISSION_LABELS: Record<PermissionId, { title: string; asking: string }> = {
  camera: { title: "Camera", asking: "use your camera" },
  "clipboard-read": { title: "Clipboard read", asking: "read your clipboard" },
  "clipboard-write": { title: "Clipboard write", asking: "write to your clipboard" },
  "display-capture": { title: "Screen capture", asking: "capture this page" },
  "file-system": { title: "File access", asking: "read and write files on your computer" },
  fullscreen: { title: "Full screen", asking: "go full screen" },
  geolocation: { title: "Location", asking: "know your location" },
  hid: { title: "HID devices", asking: "connect to a HID device" },
  "idle-detection": { title: "Idle detection", asking: "know when you are away" },
  "keyboard-lock": { title: "Keyboard lock", asking: "capture every key while full screen" },
  microphone: { title: "Microphone", asking: "use your microphone" },
  midi: { title: "MIDI", asking: "control your MIDI devices" },
  notifications: { title: "Notifications", asking: "send you notifications" },
  "open-external": { title: "Open other apps", asking: "open another application" },
  "pointer-lock": { title: "Pointer lock", asking: "capture your mouse pointer" },
  "protected-media": { title: "Protected media", asking: "play protected (DRM) media" },
  serial: { title: "Serial ports", asking: "connect to a serial port" },
  "speaker-selection": { title: "Speaker choice", asking: "choose an audio output device" },
  "storage-access": { title: "Third-party storage", asking: "use its storage inside this site" },
  usb: { title: "USB devices", asking: "connect to a USB device" },
  "window-management": { title: "Window placement", asking: "manage windows across your screens" },
};

/** "use your camera and your microphone" — one sentence for a request covering both. */
export function permissionSentence(ids: PermissionId[]): string {
  const parts = ids.map((id) => PERMISSION_LABELS[id]?.asking ?? id);
  if (parts.length === 0) return "do something this version does not recognise";
  if (parts.length === 1) return parts[0];
  return `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}

/**
 * Which of the three buttons is *the user's own* setting.
 *
 * `"default"` whenever the answer came from anywhere but them — the project
 * config, or veld. Load-bearing: showing the resolved verdict here instead made
 * the Default button unpressable on any permission `veld.json` granted, because
 * clearing the override re-resolved the row straight back to Allow and lit that
 * button up. Nothing was broken except which question the control was answering.
 */
export function userChoice(setting: PermissionSetting): "default" | "allow" | "deny" {
  return setting.source === "user" && setting.verdict !== "ask" ? setting.verdict : "default";
}

/**
 * What the permission currently resolves to, and where that came from.
 *
 * The buttons show a preference; this shows the consequence. Without it a row
 * reading "Default" is silent about whether the site may actually use the camera,
 * which is the only thing anyone opened the panel to find out.
 */
export function effectiveLabel(setting: PermissionSetting): string {
  const outcome =
    setting.verdict === "allow" ? "Allowed" : setting.verdict === "deny" ? "Blocked" : "Will ask";
  if (setting.source === "user") return ` · ${outcome} by you`;
  if (setting.source === "config") return ` · ${outcome} · set by veld.json`;
  return setting.verdict === "ask" ? "" : ` · ${outcome} · Veld default`;
}
