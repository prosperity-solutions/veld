import { MantineProvider } from "@mantine/core";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { render } from "../shared/testRender";
import { theme } from "../theme";
import { RemoteAgentCard } from "./RemoteAgent";

const hostCatalogue = {
  revision: "host-revision-1",
  runtimes: [{
    id: "host-runtime",
    label: "Host runtime",
    accounts: [
      {
        id: "host-account",
        label: "Host account",
        provider_connection_id: "host-connection",
        auth_status: "authenticated",
        identity_summary: null,
        billing_kind: "subscription",
        models: ["model-a", "model-b"].map((id) => ({
          id,
          label: id === "model-a" ? "Host model A" : "Host model B",
          options: [
            { id: "reasoningEffort", values: ["medium", "high"], default: null },
            { id: "allowTools", values: [true, false], default: null },
          ],
        })),
      },
      ...["unauthenticated", "unknown"].map((auth_status) => ({
        id: auth_status,
        label: `${auth_status} account`,
        provider_connection_id: `${auth_status}-connection`,
        auth_status,
        identity_summary: null,
        billing_kind: "unknown",
        models: [],
      })),
    ],
  }],
};

const fetchHost = vi.fn<typeof fetch>();
const scrollIntoView = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView");

beforeEach(() => {
  fetchHost.mockReset();
  fetchHost.mockResolvedValue(Response.json(hostCatalogue));
  vi.stubGlobal("fetch", fetchHost);
  // Mantine scrolls the selected option; jsdom has no layout implementation.
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  vi.unstubAllGlobals();
  if (scrollIntoView) {
    Object.defineProperty(Element.prototype, "scrollIntoView", scrollIntoView);
  } else {
    Reflect.deleteProperty(Element.prototype, "scrollIntoView");
  }
});

function startButton(): HTMLButtonElement {
  return screen.getByRole<HTMLButtonElement>("button", { name: "Start agent" });
}

async function choose(label: string, name: string): Promise<void> {
  fireEvent.click(screen.getByRole("combobox", { name: label }));
  fireEvent.click(await screen.findByRole("option", { name, hidden: true }));
}

async function open() {
  const onLaunch = vi.fn();
  render(<RemoteAgentCard worktreeId={17} onLaunch={onLaunch} />);
  fireEvent.click(screen.getByRole("button", { name: /Remote agent/ }));
  await screen.findByRole("combobox", { name: "Runtime" });
  return { onLaunch };
}

async function selectModel(): Promise<void> {
  await choose("Runtime", "Host runtime");
  await choose("Account", "Host account");
  await choose("Model", "Host model A");
  await choose("Reasoning effort", "high");
  await choose("allowTools", "false");
}

function launchRequest(index: number) {
  const calls = fetchHost.mock.calls.filter(([, init]) => init?.method === "POST");
  const [url, init] = calls[index];
  return { url, body: JSON.parse(String(init?.body)) };
}

describe("RemoteAgentCard", () => {
  test("requires explicit host selections and excludes unavailable accounts", async () => {
    const { onLaunch } = await open();
    expect(fetchHost.mock.calls[0][0]).toBe("/remote/agents/17");
    expect(screen.getByRole<HTMLInputElement>("combobox", { name: "Runtime" }).value).toBe("");
    expect(screen.getByRole<HTMLInputElement>("combobox", { name: "Account" }).disabled).toBe(true);
    expect(startButton().disabled).toBe(true);

    await choose("Runtime", "Host runtime");
    expect(startButton().disabled).toBe(true);
    fireEvent.click(screen.getByRole("combobox", { name: "Account" }));
    for (const name of ["unauthenticated account", "unknown account"]) {
      const option = await screen.findByRole("option", { name, hidden: true });
      fireEvent.click(option);
      expect(screen.getByRole<HTMLInputElement>("combobox", { name: "Account" }).value).toBe("");
    }
    fireEvent.click(screen.getByRole("option", { name: "Host account", hidden: true }));
    expect(screen.getByRole<HTMLInputElement>("combobox", { name: "Model" }).value).toBe("");
    expect(startButton().disabled).toBe(true);
    await choose("Model", "Host model A");
    expect(startButton().disabled).toBe(true);
    await choose("Reasoning effort", "high");
    expect(startButton().disabled).toBe(true);
    await choose("allowTools", "false");
    expect(startButton().disabled).toBe(false);

    fetchHost.mockResolvedValueOnce(Response.json({}));
    fireEvent.click(startButton());
    await waitFor(() => expect(onLaunch).toHaveBeenCalledTimes(1));
    const request = launchRequest(0);
    expect(request.url).toBe("/remote/agents/17/launch");
    expect(request.body).toEqual({
      session_id: expect.any(String),
      selection: {
        runtime_id: "host-runtime",
        account_id: "host-account",
        provider_connection_id: "host-connection",
        model_id: "model-a",
        catalogue_revision: "host-revision-1",
        options: { reasoningEffort: "high", allowTools: false },
      },
    });
    expect(onLaunch).toHaveBeenCalledWith({
      id: request.body.session_id, kind: "terminal", title: "Host runtime · Host model A",
    });
  });

  test("retries a failed launch with the exact session and selection", async () => {
    const { onLaunch } = await open();
    await selectModel();
    fetchHost.mockResolvedValueOnce(Response.json({ error: "Host is busy" }, { status: 503 }));
    fireEvent.click(startButton());
    expect((await screen.findByRole("alert")).textContent).toContain("Host is busy");
    expect(onLaunch).not.toHaveBeenCalled();
    expect(startButton().disabled).toBe(false);
    const first = launchRequest(0);

    fetchHost.mockResolvedValueOnce(Response.json({}));
    fireEvent.click(startButton());
    await waitFor(() => expect(onLaunch).toHaveBeenCalledTimes(1));
    expect(launchRequest(1)).toEqual(first);
    expect(onLaunch.mock.calls[0][0].id).toBe(first.body.session_id);
  });

  test.each(["model", "option"])("uses a new session after changing the %s", async (change) => {
    const { onLaunch } = await open();
    await selectModel();
    fetchHost.mockResolvedValueOnce(Response.json({ error: "Host is busy" }, { status: 503 }));
    fireEvent.click(startButton());
    await screen.findByRole("alert");
    const first = launchRequest(0);

    if (change === "model") {
      await choose("Model", "Host model B");
      expect(startButton().disabled).toBe(true);
      await choose("Reasoning effort", "high");
      await choose("allowTools", "false");
    } else {
      await choose("Reasoning effort", "medium");
    }
    fetchHost.mockResolvedValueOnce(Response.json({}));
    fireEvent.click(startButton());
    await waitFor(() => expect(onLaunch).toHaveBeenCalledTimes(1));
    const second = launchRequest(1);
    expect(second.body.session_id).not.toBe(first.body.session_id);
    expect(second.body.selection).toEqual({
      ...first.body.selection,
      ...(change === "model"
        ? { model_id: "model-b" }
        : { options: { reasoningEffort: "medium", allowTools: false } }),
    });
  });

  test("ignores a stale catalogue after switching worktrees", async () => {
    let resolveOld!: (response: Response) => void;
    fetchHost.mockReturnValueOnce(new Promise<Response>((resolve) => { resolveOld = resolve; }));
    const onLaunch = vi.fn();
    const view = render(<RemoteAgentCard worktreeId={17} onLaunch={onLaunch} />);
    fireEvent.click(screen.getByRole("button", { name: /Remote agent/ }));
    await waitFor(() => expect(fetchHost).toHaveBeenCalledTimes(1));
    const oldSignal = fetchHost.mock.calls[0][1]?.signal;

    fetchHost.mockResolvedValueOnce(Response.json({
      ...hostCatalogue,
      revision: "new-host-revision",
      runtimes: [{ ...hostCatalogue.runtimes[0], label: "New worktree runtime" }],
    }));
    view.rerender(<MantineProvider theme={theme}><RemoteAgentCard worktreeId={29} onLaunch={onLaunch} /></MantineProvider>);
    await screen.findByRole("combobox", { name: "Runtime" });
    expect(fetchHost.mock.calls[1][0]).toBe("/remote/agents/29");
    expect(oldSignal?.aborted).toBe(true);
    await choose("Runtime", "New worktree runtime");

    // A response may finish despite abort, for example after its headers arrive.
    await act(async () => { resolveOld(Response.json(hostCatalogue)); });
    expect(screen.getByRole<HTMLInputElement>("combobox", { name: "Runtime" }).value).toBe("New worktree runtime");
    await choose("Account", "Host account");
    await choose("Model", "Host model A");
    await choose("Reasoning effort", "high");
    await choose("allowTools", "false");
    fetchHost.mockResolvedValueOnce(Response.json({}));
    fireEvent.click(startButton());
    await waitFor(() => expect(onLaunch).toHaveBeenCalledTimes(1));
    expect(launchRequest(0).url).toBe("/remote/agents/29/launch");
    expect(launchRequest(0).body.selection.catalogue_revision).toBe("new-host-revision");
  });
});
