/**
 * What a failed page load means, in the terms a veld user can act on.
 *
 * Separate from the pane that renders it, and free of JSX, so it can be tested
 * without a DOM (the UI's runner is `environment: "node"`). The mapping is the
 * part worth pinning: which icon appears is cosmetic, but telling "nothing is
 * listening" apart from "that hostname does not resolve" is the difference
 * between starting the run and running `veld doctor`.
 *
 * Keyed off Chromium's net error rather than its message: the message is prose
 * that changes between versions, the codes are stable.
 */

import type { BrowserError } from "./browserHost";

export type BrowserErrorKind =
  | "unreachable"
  | "dns"
  | "timeout"
  | "cert"
  | "crash"
  | "generic";

export interface BrowserErrorCopy {
  kind: BrowserErrorKind;
  title: string;
  hint: string;
}

/** Chromium net errors, named so the switch below reads as something other than
 *  a list of magic numbers. */
const NET = {
  TIMED_OUT: -7,
  CONNECTION_CLOSED: -100,
  CONNECTION_RESET: -101,
  CONNECTION_REFUSED: -102,
  CONNECTION_ABORTED: -103,
  CONNECTION_FAILED: -104,
  NAME_NOT_RESOLVED: -105,
  INTERNET_DISCONNECTED: -106,
  ADDRESS_UNREACHABLE: -109,
  CONNECTION_TIMED_OUT: -118,
  CERT_COMMON_NAME_INVALID: -200,
  CERT_DATE_INVALID: -201,
  CERT_AUTHORITY_INVALID: -202,
  EMPTY_RESPONSE: -324,
} as const;

export function describeBrowserError(err: BrowserError): BrowserErrorCopy {
  if (err.kind === "crash") {
    return {
      kind: "crash",
      title: "The page crashed",
      hint: `Its renderer stopped (${err.text}). Reloading starts a fresh one.`,
    };
  }
  if (
    err.kind === "cert" ||
    (err.code !== null && err.code <= NET.CERT_COMMON_NAME_INVALID && err.code >= -299)
  ) {
    return {
      kind: "cert",
      title: "The certificate isn't trusted",
      hint: "Veld serves runs with Caddy's local CA. Run `veld doctor` to check it is in the system trust store.",
    };
  }
  switch (err.code) {
    // The everyday one: the URL and the route exist, the dev server does not.
    case NET.CONNECTION_REFUSED:
    case NET.CONNECTION_RESET:
    case NET.CONNECTION_CLOSED:
    case NET.CONNECTION_ABORTED:
    case NET.CONNECTION_FAILED:
    case NET.ADDRESS_UNREACHABLE:
    case NET.EMPTY_RESPONSE:
      return {
        kind: "unreachable",
        title: "Nothing is listening here",
        hint: "The address resolves but nothing answered. Is the run started?",
      };
    // The hostname itself, which for a veld URL is veld's DNS or the helper.
    case NET.NAME_NOT_RESOLVED:
    case NET.INTERNET_DISCONNECTED:
      return {
        kind: "dns",
        title: "That hostname doesn't resolve",
        hint: "Veld's DNS or the privileged helper may not be set up. `veld doctor` reports on both.",
      };
    case NET.CONNECTION_TIMED_OUT:
    case NET.TIMED_OUT:
      return {
        kind: "timeout",
        title: "The server didn't answer",
        hint: "It accepted the connection but never replied. It may still be starting up.",
      };
    default:
      return {
        kind: "generic",
        title: "This page couldn't be loaded",
        // The code is worth showing: it is the searchable part, and this branch
        // exists precisely for the errors we have no advice for.
        hint: err.code === null ? err.text : `${err.text} (${err.code})`,
      };
  }
}
