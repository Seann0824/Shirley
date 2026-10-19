import { useEffect, useRef, useState } from "react";
import { Download, LoaderCircle, Plus, RefreshCw, Trash2 } from "lucide-react";
import { agentBridge } from "@/lib/bridge";
import { emptyModel, emptyProvider, type ModelDefinition, type ProviderDefinition, type ProviderRegistry, type ProviderView, type ProviderProtocol } from "@/lib/model-providers";
import { Button } from "@/ui/button";
import { Select } from "@/ui/select";
import { cn } from "@/ui/utils";

const field = "mt-1 w-full rounded-control border border-line bg-canvas px-2.5 py-2 text-body-sm text-ink focus:outline-2 focus:outline-ink disabled:opacity-50";

function draftOf(provider: ProviderView): ProviderDefinition {
  const { has_api_key: _, ...draft } = provider;
  return { ...structuredClone(draft), api_key: null };
}

/**
 * Provider / model configuration, embedded inside the unified Settings dialog.
 *
 * Same logic as the former standalone ProviderManager dialog (CRUD + model list +
 * fetch-from-endpoint + import legacy), rendered as a full-width settings page rather
 * than a nested modal. On save it dispatches `model-providers-updated` so the composer's
 * model selector refreshes.
 */
export function ProviderSettingsPanel() {
  const [registry, setRegistry] = useState<ProviderRegistry | null>(null);
  const [draft, setDraft] = useState<ProviderDefinition>(emptyProvider);
  const [key, setKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [makeDefault, setMakeDefault] = useState(false);
  const [extraBody, setExtraBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [scope, setScope] = useState("");
  const [fetched, setFetched] = useState<ModelDefinition[]>([]);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const loaded = useRef(false);
  const saved = registry?.providers.find((p) => p.id === draft.id);
  const currentDefault = !!registry?.default_selection?.startsWith(`provider:${draft.id}:`);
  const routeChanged = saved && (saved.base_url !== draft.base_url.trim().replace(/\/+$/, "") || saved.protocol !== draft.protocol);

  const edit = (provider: ProviderDefinition, state: ProviderRegistry | null = registry) => {
    setDraft(structuredClone(provider)); setKey(""); setClearKey(false); setError(""); setNotice("");
    setExtraBody(provider.extra_body ? JSON.stringify(provider.extra_body, null, 2) : "");
    setMakeDefault(!state?.default_selection || !!state.default_selection.startsWith(`provider:${provider.id}:`));
    setFetched([]); setChosen(new Set());
  };
  const reload = async () => {
    setBusy(true); setError("");
    try {
      const bridge = await agentBridge();
      setScope(({ local: "This machine", remote: "Remote", demo: "Demo mode: config is not written locally and no real model is called" })[bridge.capabilities().configurationScope]);
      const next = await bridge.providers.get(); setRegistry(next);
      const provider = next.providers.find((p) => p.id === draft.id) ?? next.providers[0];
      edit(provider ? draftOf(provider) : emptyProvider(), next);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const inputProvider = () => ({ ...draft, api_key: key || null, extra_body: extraBody.trim() ? JSON.parse(extraBody) as Record<string, unknown> : null });
  const changed = (next: ProviderRegistry) => {
    setRegistry(next);
    window.dispatchEvent(new CustomEvent("model-providers-updated"));
  };
  const save = async () => {
    if (!registry) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const next = await (await agentBridge()).providers.save({ revision: registry.revision, provider: inputProvider(), clear_api_key: clearKey, set_default: makeDefault });
      changed(next);
      const provider = next.providers.find((p) => p.id === draft.id)!;
      edit(draftOf(provider), next);
      setNotice("Saved. Existing chats keep their current connection; apply it by picking it in the model list — new chats use the default model.");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const remove = async () => {
    if (!registry || !saved || !window.confirm(`Delete “${saved.name}”? Chats using it will need to pick a model again.`)) return;
    setBusy(true); setError("");
    try {
      const next = await (await agentBridge()).providers.delete(registry.revision, saved.id); changed(next);
      edit(next.providers[0] ? draftOf(next.providers[0]) : emptyProvider(), next);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const importLegacy = async () => {
    if (!registry) return;
    setBusy(true); setError("");
    try {
      const next = await (await agentBridge()).providers.importLegacy(registry.revision); changed(next);
      edit(draftOf(next.providers[next.providers.length - 1]), next);
      setNotice("Imported the current connection and default model. You can keep adding models manually or fetch them from the endpoint.");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const fetchModels = async () => {
    setBusy(true); setError(""); setNotice("");
    try {
      const entries = await (await agentBridge()).providers.fetchModels(inputProvider(), clearKey);
      setFetched(entries); setChosen(new Set());
      if (!entries.length) setNotice("The endpoint returned no models; please add them manually.");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const updateModel = (index: number, patch: Partial<ModelDefinition>) => setDraft((old) => {
    const models = old.models.map((model, i) => i === index ? { ...model, ...patch } : model);
    const default_model = patch.id !== undefined && old.default_model === old.models[index].id ? patch.id : old.default_model;
    return { ...old, models, default_model };
  });
  const addFetched = () => {
    const additions = fetched.filter((model) => chosen.has(model.id) && !draft.models.some((old) => old.id === model.id));
    setDraft((old) => {
      const models = [...old.models.filter((model) => model.id.trim()), ...additions];
      return { ...old, models, default_model: old.default_model || models[0]?.id || "" };
    });
    setFetched([]); setChosen(new Set());
  };

  return (
    <div className="flex min-h-0 flex-col gap-4">
      <p className="text-label text-muted">{scope} · Connection and keys are stored on the current runtime</p>
      <div className="grid gap-4 lg:grid-cols-[13rem_minmax(0,1fr)]">
        <aside className="flex flex-col gap-2">
          <Button size="sm" className="w-full" disabled={busy} onClick={() => edit(emptyProvider())}><Plus />Add provider</Button>
          <div className="flex gap-1 overflow-auto lg:flex-col">
            {registry?.providers.map((provider) => (
              <button
                type="button"
                key={provider.id}
                disabled={busy}
                onClick={() => edit(draftOf(provider))}
                className={cn("min-w-28 rounded-control px-3 py-2 text-left text-body-sm hover:bg-selected", provider.id === draft.id && "bg-selected")}
              >
                <span className="block truncate">{provider.name}</span>
                {registry.default_selection?.startsWith(`provider:${provider.id}:`) && <span className="text-caption text-muted">New chat default</span>}
              </button>
            ))}
          </div>
          <Button size="sm" variant="ghost" className="w-full" disabled={busy || !registry} onClick={() => void importLegacy()}>Import existing connection</Button>
        </aside>
        <form className="min-w-0 space-y-4" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="text-body-sm">Provider name<input className={field} required disabled={busy} value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
            <label className="text-body-sm">Protocol<Select<ProviderProtocol> className="mt-1 w-full max-w-none" label="Protocol" disabled={busy} value={draft.protocol} onChange={(next) => setDraft({ ...draft, protocol: next })} options={[{ value: "chat_completions", label: "Chat Completions" }, { value: "responses", label: "Responses" }, { value: "anthropic_messages", label: "Anthropic Messages" }]} /></label>
          </div>
          <label className="block text-body-sm">Endpoint<input className={field} type="url" required disabled={busy} placeholder="https://api.example.com/v1" value={draft.base_url} onChange={(e) => setDraft({ ...draft, base_url: e.target.value })} /><span className="mt-1 block text-caption text-muted">Accepts a service root, /v1, or a full endpoint URL.</span></label>
          <label className="block text-body-sm">API Key<input className={field} type="password" autoComplete="new-password" disabled={busy || clearKey} placeholder={saved?.has_api_key && !routeChanged ? "Saved; leave blank to keep the current key" : "Leave blank for local services without auth"} value={key} onChange={(e) => setKey(e.target.value)} /></label>
          {saved?.has_api_key && <label className="flex items-center gap-2 text-caption text-muted"><input type="checkbox" disabled={busy} checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />Clear the saved key</label>}
          {routeChanged && saved?.has_api_key && !key && !clearKey && <p className="text-caption text-muted">Endpoint or protocol changed, so the old key is not reused; re-enter it if authentication is required.</p>}
          <details className="text-body-sm"><summary className="cursor-pointer text-muted">Advanced connection settings</summary><div className="mt-3 space-y-3">
            <label className="block">Model list URL (optional)<input className={field} type="url" disabled={busy} placeholder="Leave blank to derive /models automatically" value={draft.models_url || ""} onChange={(e) => setDraft({ ...draft, models_url: e.target.value || null })} /></label>
            <label className="block">Extra request body (JSON object, optional)<textarea className={cn(field, "min-h-24 font-mono")} disabled={busy} placeholder='{"enable_thinking": false}' value={extraBody} onChange={(e) => setExtraBody(e.target.value)} /></label>
          </div></details>
          <section className="border-t border-line pt-3" aria-label="Provider model list">
            <div className="mb-3 flex flex-wrap items-center gap-2"><h3 className="mr-auto text-body-sm font-medium">Models</h3><Button size="sm" disabled={busy} onClick={() => setDraft((old) => ({ ...old, models: [...old.models, emptyModel()] }))}><Plus />Add manually</Button><Button size="sm" disabled={busy || !draft.base_url} onClick={() => void fetchModels()}><Download />Fetch from endpoint</Button></div>
            {fetched.length > 0 && <div className="mb-3 rounded-control border border-line p-3"><p className="mb-2 text-caption text-muted">Select the models to add. Fetching the list only checks the list endpoint; it does not verify model inference.</p><div className="max-h-44 overflow-auto">
              {fetched.map((model) => <label key={model.id} className="flex items-center gap-2 py-1 text-body-sm"><input type="checkbox" checked={chosen.has(model.id)} disabled={draft.models.some((old) => old.id === model.id)} onChange={(event) => setChosen((old) => { const next = new Set(old); if (event.target.checked) next.add(model.id); else next.delete(model.id); return next; })} /><span className="break-all">{model.name || model.id}</span></label>)}
            </div><Button size="sm" className="mt-2" disabled={!chosen.size} onClick={addFetched}>Add selected models</Button></div>}
            <div className="space-y-3">{draft.models.map((model, index) => <div key={index} className="rounded-control border border-line p-3">
              <div className="flex items-start gap-2"><div className="grid min-w-0 flex-1 gap-2 sm:grid-cols-2"><label className="text-caption text-muted">Model ID<input className={field} required disabled={busy} placeholder="Model ID used in actual requests" value={model.id} onChange={(e) => updateModel(index, { id: e.target.value })} /></label><label className="text-caption text-muted">Display name<input className={field} disabled={busy} placeholder="Uses the model ID when blank" value={model.name} onChange={(e) => updateModel(index, { name: e.target.value })} /></label></div><button type="button" aria-label={`Remove model ${model.id || index + 1}`} disabled={busy} className="mt-6 rounded-control p-2 text-muted hover:bg-selected hover:text-danger-ink" onClick={() => setDraft((old) => { const models = old.models.filter((_, i) => i !== index); return { ...old, models, default_model: old.default_model === model.id ? models[0]?.id || "" : old.default_model }; })}><Trash2 className="size-4" /></button></div>
              <details className="mt-2 text-caption text-muted"><summary className="cursor-pointer">Capabilities · context {model.context_window_tokens.toLocaleString()}{model.supports_images ? " · images" : ""}</summary><div className="mt-2 grid gap-3 sm:grid-cols-2">
                <label>Context length<input className={field} type="number" min="1" max="4294967295" step="1" required disabled={busy} value={model.context_window_tokens || ""} onChange={(e) => updateModel(index, { context_window_tokens: Number(e.target.value) })} /></label>
                <label>Tool calling<Select<ModelDefinition["tool_mode"]> className="mt-1 w-full max-w-none" label="Tool calling" disabled={busy} value={model.tool_mode} onChange={(next) => updateModel(index, { tool_mode: next })} options={[{ value: "native", label: "Native tool calls" }, { value: "content", label: "Text tool calls" }]} /></label>
                <label className="flex items-center gap-2"><input type="checkbox" disabled={busy} checked={model.supports_images} onChange={(e) => updateModel(index, { supports_images: e.target.checked })} />Supports images</label>
                <label className="flex items-center gap-2"><input type="checkbox" disabled={busy} checked={model.thinking} onChange={(e) => updateModel(index, { thinking: e.target.checked })} />Enable thinking</label>
                <label>Reasoning effort<Select<NonNullable<ModelDefinition["reasoning_effort"]> | ""> className="mt-1 w-full max-w-none" label="Reasoning effort" disabled={busy} value={model.reasoning_effort || ""} onChange={(next) => updateModel(index, { reasoning_effort: next === "" ? null : next })} options={[{ value: "", label: "Unspecified" }, { value: "low", label: "Low" }, { value: "medium", label: "Medium" }, { value: "high", label: "High" }]} /></label>
              </div></details>
            </div>)}</div>
          </section>
          <label className="block text-body-sm">Provider default model<Select<string> className="mt-1 w-full max-w-none" label="Provider default model" disabled={busy} value={draft.default_model} onChange={(next) => setDraft({ ...draft, default_model: next })} placeholder="Select…" options={draft.models.filter((m) => m.id.trim()).map((model) => ({ value: model.id, label: model.name || model.id }))} /></label>
          <label className="flex items-center gap-2 text-body-sm"><input type="checkbox" disabled={busy || currentDefault} checked={makeDefault} onChange={(e) => setMakeDefault(e.target.checked)} />{currentDefault ? "Currently the new-chat default; change it in another provider" : "Set as the default provider and model for new chats"}</label>
          {error && <p role="alert" className="break-words text-body-sm text-danger-ink">{error}</p>}{notice && <p role="status" className="text-body-sm text-muted">{notice}</p>}
          <footer className="flex flex-wrap items-center gap-2 border-t border-line pt-3"><Button type="submit" variant="primary" size="sm" disabled={busy || !registry}>{busy ? <LoaderCircle className="animate-spin" /> : null}Save config</Button><Button size="sm" disabled={busy} onClick={() => void reload()}><RefreshCw />Reload</Button>{saved && <Button size="sm" variant="danger" className="ml-auto" disabled={busy} onClick={() => void remove()}><Trash2 />Delete provider</Button>}</footer>
        </form>
      </div>
    </div>
  );
}
