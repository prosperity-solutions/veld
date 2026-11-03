import { describe, expect, it, vi } from "vitest";
import { createEngineMutationClient, PendingActionError } from "./engineMutation";

const FIRST_KEY = "11111111-1111-4111-8111-111111111111";
const SECOND_KEY = "22222222-2222-4222-8222-222222222222";
const accepted = () => new Response('{"action_id":"operation-one"}', { status: 202 });

function prepare() {
  const saved = new Map<string, string>();
  const storage = {
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => { saved.set(key, value); },
    removeItem: (key: string) => { saved.delete(key); },
  };
  const fetch = vi.fn<typeof globalThis.fetch>();
  const randomUUID = vi.fn().mockReturnValueOnce(FIRST_KEY).mockReturnValue(SECOND_KEY);
  const now = vi.fn().mockReturnValue(1000);
  const options = { storage, fetch, randomUUID, now };
  const errorMessage = (response: Response) => response.text();
  const submit = createEngineMutationClient(errorMessage, options);
  const keyAt = (index: number) => new Headers(fetch.mock.calls[index][1]?.headers).get("Idempotency-Key");
  return { saved, storage, fetch, randomUUID, now, options, errorMessage, submit, keyAt };
}

describe("engine mutation retries", () => {
  it("pins an ambiguous start tuple even when the next run name and preset change", async () => {
    const p = prepare();
    const original = { preset: "dev", selections: ["web:local"], run_name: "dev" };
    p.fetch.mockRejectedValueOnce(new TypeError("response lost after dispatch"));
    await expect(p.submit("/api/worktrees/42/start", JSON.stringify(original))).rejects.toBeInstanceOf(PendingActionError);
    expect(p.submit.pendingStart(42)).toEqual(original);
    expect(p.fetch).toHaveBeenCalledTimes(1);
    expect(p.randomUUID).toHaveBeenCalledTimes(1);

    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/api/worktrees/42/start", '{"preset":"changed","run_name":"dev-2"}');
    expect(JSON.parse(p.fetch.mock.calls[1][1]?.body as string)).toEqual(original);
    expect(p.keyAt(1)).toBe(FIRST_KEY);
    expect(p.submit.pendingStart(42)).toBeNull();

    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/api/worktrees/42/start", '{"preset":"changed","run_name":"dev-2"}');
    expect(p.keyAt(2)).toBe(SECOND_KEY);
    expect(JSON.parse(p.fetch.mock.calls[2][1]?.body as string).run_name).toBe("dev-2");
  });

  it("restores a public start intent from persistent storage in a new client", async () => {
    const p = prepare();
    const original = { preset: "dev", run_name: "dev" };
    p.fetch.mockRejectedValueOnce(new TypeError("offline"));
    await expect(p.submit("/api/worktrees/42/start", JSON.stringify(original))).rejects.toBeInstanceOf(PendingActionError);
    const reloaded = createEngineMutationClient(p.errorMessage, p.options);
    expect(reloaded.pendingStart(42)).toEqual(original);
    expect(JSON.parse([...p.saved.values()][0])).toEqual({ key: FIRST_KEY, createdAt: 1000, start: original });
    expect(reloaded.pendingStart(43)).toBeNull();
    p.fetch.mockResolvedValueOnce(accepted());
    await reloaded("/api/worktrees/42/start", JSON.stringify(reloaded.pendingStart(42)));
    expect(p.keyAt(1)).toBe(FIRST_KEY);
    expect(p.randomUUID).toHaveBeenCalledTimes(1);
  });

  it("shares an in-flight start even when another caller proposes a new name", async () => {
    const p = prepare();
    p.fetch.mockResolvedValueOnce(accepted());
    await Promise.all([
      p.submit("/api/worktrees/42/start", '{"run_name":"dev"}'),
      p.submit("/api/worktrees/42/start", '{"run_name":"dev-2"}'),
    ]);
    expect(p.fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(p.fetch.mock.calls[0][1]?.body as string).run_name).toBe("dev");
    expect(p.randomUUID).toHaveBeenCalledTimes(1);
  });

  it("does not persist credentials or unknown fields from a start request", async () => {
    const p = prepare();
    await expect(p.submit("/api/worktrees/42/start", '{"run_name":"dev","secrets":{"token":"synthetic-only"}}')).rejects.toThrow("pending start request is invalid");
    expect(p.saved.size).toBe(0);
    expect(p.fetch).not.toHaveBeenCalled();
  });

  it("does not replace a damaged pinned start with a fresh key", async () => {
    const p = prepare();
    p.storage.setItem("veld.engine.pending.start.42", JSON.stringify({ key: FIRST_KEY, createdAt: 1000 }));
    expect(() => p.submit.pendingStart(42)).toThrow("pending start request is unavailable");
    await expect(p.submit("/api/worktrees/42/start", '{"run_name":"dev-2"}')).rejects.toThrow("pending start request is unavailable");
    expect(p.randomUUID).not.toHaveBeenCalled();
    expect(p.fetch).not.toHaveBeenCalled();
  });

  it("retains a transport failure key and clears it after a receipt", async () => {
    const p = prepare();
    p.fetch.mockRejectedValueOnce(new TypeError("connection lost"));
    await expect(p.submit("/api/worktrees/42/start", '{"preset":"dev"}')).rejects.toBeInstanceOf(PendingActionError);
    p.fetch.mockResolvedValueOnce(accepted());
    await expect(p.submit("/api/worktrees/42/start", '{"preset":"dev"}')).resolves.toEqual({ action_id: "operation-one" });
    expect(p.keyAt(0)).toBe(FIRST_KEY);
    expect(p.keyAt(1)).toBe(FIRST_KEY);
    expect(p.saved.size).toBe(0);
    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/api/worktrees/42/start", '{"preset":"dev"}');
    expect(p.keyAt(2)).toBe(SECOND_KEY);
  });

  it("recovers after reload without persisting the request or credentials", async () => {
    const p = prepare();
    p.fetch.mockRejectedValueOnce(new TypeError("offline"));
    const body = '{"action":"public-action-name","node":"web"}';
    await expect(p.submit("/api/environments/dev/action", body)).rejects.toBeInstanceOf(PendingActionError);
    const persisted = JSON.stringify([...p.saved]);
    expect(persisted).not.toContain("public-action-name");
    expect(persisted).not.toContain("/api/environments");
    expect(JSON.parse([...p.saved.values()][0])).toEqual({ key: FIRST_KEY, createdAt: 1000 });
    const reloaded = createEngineMutationClient(p.errorMessage, p.options);
    p.fetch.mockResolvedValueOnce(accepted());
    await reloaded("/api/environments/dev/action", body);
    expect(p.keyAt(1)).toBe(FIRST_KEY);
    expect(p.randomUUID).toHaveBeenCalledTimes(1);
  });

  it("shares one in-flight request for concurrent identical submissions", async () => {
    const p = prepare();
    p.fetch.mockResolvedValue(accepted());
    const [one, two] = await Promise.all([p.submit("/stop"), p.submit("/stop")]);
    expect(one).toEqual(two);
    expect(p.fetch).toHaveBeenCalledTimes(1);
  });

  it("clears a definitive initial rejection", async () => {
    const p = prepare();
    p.fetch.mockResolvedValueOnce(new Response("unknown preset", { status: 422 }));
    await expect(p.submit("/start")).rejects.toThrow("unknown preset");
    expect(p.saved.size).toBe(0);
    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/start");
    expect(p.keyAt(1)).toBe(SECOND_KEY);
  });

  it("keeps an uncertain key through gateway failures and rejected recovery", async () => {
    const p = prepare();
    p.fetch.mockResolvedValueOnce(new Response("upstream lost", { status: 502 }));
    await expect(p.submit("/restart")).rejects.toBeInstanceOf(PendingActionError);
    p.fetch.mockResolvedValueOnce(new Response("reauthenticate", { status: 401 }));
    await expect(p.submit("/restart")).rejects.toBeInstanceOf(PendingActionError);
    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/restart");
    expect([p.keyAt(0), p.keyAt(1), p.keyAt(2)]).toEqual([FIRST_KEY, FIRST_KEY, FIRST_KEY]);
  });

  it("retains the key when a successful response body is lost", async () => {
    const p = prepare();
    p.fetch.mockResolvedValueOnce(new Response("", { status: 202 }));
    await expect(p.submit("/stop")).rejects.toBeInstanceOf(PendingActionError);
    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/stop");
    expect(p.keyAt(1)).toBe(FIRST_KEY);
  });

  it("refuses to send when pending storage fails", async () => {
    const p = prepare();
    p.storage.setItem = () => { throw new Error("storage unavailable"); };
    await expect(p.submit("/stop")).rejects.toThrow("storage unavailable");
    expect(p.fetch).not.toHaveBeenCalled();
  });

  it("refuses to replay beyond the server retention window", async () => {
    const p = prepare();
    p.fetch.mockRejectedValueOnce(new TypeError("offline"));
    await expect(p.submit("/stop")).rejects.toBeInstanceOf(PendingActionError);
    p.now.mockReturnValue(1000 + 7 * 24 * 60 * 60 * 1000);
    await expect(p.submit("/stop")).rejects.toThrow("retry window has expired");
    expect(p.fetch).toHaveBeenCalledTimes(1);
    expect(p.saved.size).toBe(1);
  });

  it("uses distinct keys for different paths or payloads", async () => {
    const p = prepare();
    p.fetch.mockRejectedValueOnce(new TypeError("offline"));
    await expect(p.submit("/action", '{"action":"one"}')).rejects.toBeInstanceOf(PendingActionError);
    p.fetch.mockResolvedValueOnce(accepted());
    await p.submit("/action", '{"action":"two"}');
    expect(p.keyAt(1)).toBe(SECOND_KEY);
  });
});
