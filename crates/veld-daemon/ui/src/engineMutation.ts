export interface ActionReceipt {
  action_id: string;
}

export interface RunStartRequest {
  preset?: string;
  selections?: string[];
  run_name?: string;
}

type RetryStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;
interface PendingRequest {
  key: string;
  createdAt: number;
  start?: RunStartRequest;
}

const RETRY_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;
const STORAGE_PREFIX = "veld.engine.pending.";
const UUID = /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i;
const START_ROUTE = /^\/api\/worktrees\/(\d+)\/start$/;

function publicStart(value: unknown): RunStartRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("The pending start request is invalid. Check the run state before proceeding.");
  }
  const fields = value as Record<string, unknown>;
  if (Object.keys(fields).some(key => !["preset", "selections", "run_name"].includes(key))
    || (fields.preset !== undefined && typeof fields.preset !== "string")
    || (fields.run_name !== undefined && typeof fields.run_name !== "string")
    || (fields.selections !== undefined && (!Array.isArray(fields.selections)
      || !fields.selections.every(selection => typeof selection === "string")))) {
    throw new Error("The pending start request is invalid. Check the run state before proceeding.");
  }
  return {
    ...(fields.preset === undefined ? {} : { preset: fields.preset as string }),
    ...(fields.selections === undefined ? {} : { selections: [...(fields.selections as string[])] }),
    ...(fields.run_name === undefined ? {} : { run_name: fields.run_name as string }),
  };
}

/** Preserve the original key when dispatch or acknowledgement is uncertain. */
export class PendingActionError extends Error {
  constructor(
    message: string,
    readonly idempotencyKey: string,
    cause?: unknown,
  ) {
    super(message, { cause });
    this.name = "PendingActionError";
  }
}

/** Only public run-start, stop, restart and action payloads belong here. */
export function createEngineMutationClient(
  errorMessage: (response: Response) => Promise<string>,
  options: {
    storage?: RetryStorage;
    fetch?: typeof fetch;
    randomUUID?: () => string;
    now?: () => number;
  } = {},
) {
  const active = new Map<string, Promise<ActionReceipt>>();
  const retryStorage = () => options.storage ?? globalThis.localStorage;
  const startStorageKey = (worktreeId: number) => `${STORAGE_PREFIX}start.${worktreeId}`;

  function readPending(storageKey: string): PendingRequest | null {
    const stored = retryStorage().getItem(storageKey);
    if (stored === null) return null;
    const pending: PendingRequest = JSON.parse(stored);
    if (!pending || !UUID.test(pending.key) || !Number.isFinite(pending.createdAt)) {
      throw new Error("The pending action receipt is invalid. Check the run state before proceeding.");
    }
    return {
      key: pending.key,
      createdAt: pending.createdAt,
      ...(pending.start === undefined ? {} : { start: publicStart(pending.start) }),
    };
  }

  function pendingStart(worktreeId: number): RunStartRequest | null {
    const pending = readPending(startStorageKey(worktreeId));
    if (pending === null) return null;
    if (pending.start === undefined) {
      throw new Error("The pending start request is unavailable. Check the run state before proceeding.");
    }
    return pending.start;
  }

  async function send(storageKey: string, path: string, body?: string, starting = false): Promise<ActionReceipt> {
    const storage = retryStorage();
    const now = (options.now ?? Date.now)();
    const stored = readPending(storageKey);
    const pending: PendingRequest = stored ?? {
      key: (options.randomUUID ?? (() => crypto.randomUUID()))(),
      createdAt: now,
      ...(starting ? { start: publicStart(JSON.parse(body ?? "{}")) } : {}),
    };
    if (starting && pending.start === undefined) {
      throw new Error("The pending start request is unavailable. Check the run state before proceeding.");
    }
    if (now - pending.createdAt >= RETRY_WINDOW_MS) {
      throw new PendingActionError(
        "The action retry window has expired. Check the run state before submitting another action.",
        pending.key,
      );
    }
    // Persist before sending. Storage failure must prevent an untracked mutation.
    // Starts also retain their public tuple: a run list can change the next name
    // before a retry. No credentials or secret values belong in this record.
    storage.setItem(storageKey, JSON.stringify(pending));
    try {
      const response = await (options.fetch ?? globalThis.fetch)(path, {
        method: "POST",
        headers: {
          "X-Veld-Request": "1",
          "Idempotency-Key": pending.key,
          ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        },
        body: starting ? JSON.stringify(pending.start) : body,
      });
      if (!response.ok) {
        const message = await errorMessage(response);
        // A rejection of a recovery request cannot disprove an earlier dispatch.
        if (!stored && response.status >= 400 && response.status < 500) {
          storage.removeItem(storageKey);
          throw new Error(message);
        }
        throw new PendingActionError(message, pending.key);
      }
      const receipt: unknown = await response.json();
      if (
        !receipt || typeof receipt !== "object" || !("action_id" in receipt)
        || typeof receipt.action_id !== "string" || !receipt.action_id
      ) {
        throw new Error("The server did not return an action receipt.");
      }
      storage.removeItem(storageKey);
      return { action_id: receipt.action_id };
    } catch (error) {
      if (error instanceof PendingActionError || storage.getItem(storageKey) === null) throw error;
      throw new PendingActionError(
        "The action outcome is unknown. Retry the same action to recover its receipt.",
        pending.key,
        error,
      );
    }
  }

  const submit = async (path: string, body?: string): Promise<ActionReceipt> => {
    const start = START_ROUTE.exec(path);
    const signature = JSON.stringify(["POST", path, start ? null : body ?? null]);
    const running = active.get(signature);
    if (running) return running;
    const request = (async () => {
      if (start) return send(startStorageKey(Number(start[1])), path, body, true);
      const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(signature));
      const fingerprint = Array.from(
        new Uint8Array(digest),
        byte => byte.toString(16).padStart(2, "0"),
      ).join("");
      return send(STORAGE_PREFIX + fingerprint, path, body);
    })();
    active.set(signature, request);
    try {
      return await request;
    } finally {
      active.delete(signature);
    }
  };
  return Object.assign(submit, { pendingStart });
}
