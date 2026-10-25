import { Alert, Button, Group, Loader, Modal, Select, Stack, Text } from "@mantine/core";
import { IconTerminal2 } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { newTabId, type PaneTab } from "./model";

type OptionValue = string | boolean;
interface Model {
  id: string;
  label: string;
  options: Array<{ id: string; values: OptionValue[]; default: OptionValue | null }>;
}
interface Account {
  id: string;
  label: string;
  provider_connection_id: string;
  auth_status: "authenticated" | "unauthenticated" | "unknown";
  identity_summary: string | null;
  billing_kind: string;
  models: Model[];
}
interface Catalogue {
  revision: string;
  runtimes: Array<{ id: string; label: string; accounts: Account[] }>;
}

async function responseValue<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const value = await response.json();
    throw new Error(typeof value.error === "string" ? value.error : "The runner refused the request.");
  }
  return response.json() as Promise<T>;
}

/** The runner discovers accounts and validates the complete selection again at launch. */
export function RemoteAgentCard(props: { worktreeId: number; onLaunch: (tab: PaneTab) => void }) {
  const [opened, setOpened] = useState(false);
  const [catalogue, setCatalogue] = useState<Catalogue | null>(null);
  const [runtimeId, setRuntimeId] = useState<string | null>(null);
  const [accountId, setAccountId] = useState<string | null>(null);
  const [modelId, setModelId] = useState<string | null>(null);
  const [options, setOptions] = useState<Record<string, OptionValue>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const sessionId = useRef(newTabId());
  const generation = useRef(0);
  const runtime = catalogue?.runtimes.find((item) => item.id === runtimeId);
  const account = runtime?.accounts.find((item) => item.id === accountId);
  const model = account?.models.find((item) => item.id === modelId);

  useEffect(() => {
    const current = ++generation.current;
    setCatalogue(null);
    setRuntimeId(null);
    setAccountId(null);
    setModelId(null);
    setOptions({});
    setError(null);
    setBusy(false);
    sessionId.current = newTabId();
    if (!opened) return;
    const controller = new AbortController();
    fetch(`/remote/agents/${props.worktreeId}`, { signal: controller.signal })
      .then(responseValue<Catalogue>)
      .then((value) => { if (current === generation.current) setCatalogue(value); })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted && current === generation.current) {
          setError(cause instanceof Error ? cause.message : "Account discovery failed.");
        }
      });
    return () => { controller.abort(); ++generation.current; };
  }, [opened, props.worktreeId, refresh]);

  async function launch() {
    if (!catalogue || !runtime || !account || !model || busy) return;
    const current = generation.current;
    const id = sessionId.current;
    setBusy(true);
    setError(null);
    try {
      await fetch(`/remote/agents/${props.worktreeId}/launch`, {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Veld-Request": "1" },
        body: JSON.stringify({ session_id: id, selection: {
          runtime_id: runtime.id, account_id: account.id,
          provider_connection_id: account.provider_connection_id,
          model_id: model.id, catalogue_revision: catalogue.revision, options,
        } }),
      }).then(responseValue);
      if (current !== generation.current) return;
      props.onLaunch({ id, kind: "terminal", title: `${runtime.label} · ${model.label}` });
      setOpened(false);
    } catch (cause) {
      if (current === generation.current) setError(cause instanceof Error ? cause.message : "Agent launch failed.");
    } finally {
      if (current === generation.current) setBusy(false);
    }
  }

  return <>
    <button className="pane-card" onClick={() => setOpened(true)}>
      <span className="pane-card-main"><IconTerminal2 size={15} /> Remote agent</span>
      <span className="pane-card-sub">Choose a host account and model</span>
    </button>
    <Modal opened={opened} onClose={() => { if (!busy) setOpened(false); }} title="Start a remote agent" centered>
      <Stack gap="sm">
        <Text size="sm">Runs on {window.location.hostname}. Credentials stay on that machine.</Text>
        {error && <Alert color="red" role="alert">{error}</Alert>}
        {!catalogue && !error && <Loader size="sm" aria-label="Discovering host accounts" />}
        {catalogue && <>
          <Select label="Runtime" placeholder="Select a runtime" value={runtimeId}
            disabled={busy} data={catalogue.runtimes.map((item) => ({ value: item.id, label: item.label }))}
            onChange={(value) => { setRuntimeId(value); setAccountId(null); setModelId(null); setOptions({}); sessionId.current = newTabId(); }} />
          <Select label="Account" placeholder="Select a host account" value={accountId} disabled={!runtime || busy}
            data={runtime?.accounts.map((item) => ({ value: item.id, label: item.label,
              disabled: item.auth_status !== "authenticated" })) ?? []}
            onChange={(value) => { setAccountId(value); setModelId(null); setOptions({}); sessionId.current = newTabId(); }} />
          {account && <Text size="xs" c="dimmed">{account.identity_summary ?? account.label} · {account.billing_kind}</Text>}
          <Text size="xs" c="dimmed">This trial supports the inherited Codex account. Other account selectors remain unavailable.</Text>
          <Select label="Model" placeholder="Select a model" value={modelId} disabled={!account || busy}
            data={account?.models.map((item) => ({ value: item.id, label: item.label })) ?? []}
            onChange={(value) => {
              setModelId(value); sessionId.current = newTabId();
              const selected = account?.models.find((item) => item.id === value);
              setOptions(Object.fromEntries(selected?.options.filter((item) => item.default !== null)
                .map((item) => [item.id, item.default as OptionValue]) ?? []));
            }} />
          {model?.options.map((option) => <Select key={option.id} label={option.id === "reasoningEffort" ? "Reasoning effort" : option.id === "serviceTier" ? "Service tier" : option.id}
            disabled={busy} placeholder="Select an option" value={options[option.id] === undefined ? null : JSON.stringify(options[option.id])}
            data={option.values.map((value) => ({ value: JSON.stringify(value), label: String(value) }))}
            onChange={(value) => {
              if (value !== null) { setOptions((previous) => ({ ...previous, [option.id]: JSON.parse(value) as OptionValue })); sessionId.current = newTabId(); }
            }} />)}
        </>}
        <Group justify="space-between">
          <Button variant="subtle" disabled={busy} onClick={() => setRefresh((value) => value + 1)}>Refresh accounts</Button>
          <Button loading={busy} disabled={!model || account?.auth_status !== "authenticated" || model.options.some((item) => options[item.id] === undefined)} onClick={() => void launch()}>Start agent</Button>
        </Group>
      </Stack>
    </Modal>
  </>;
}
