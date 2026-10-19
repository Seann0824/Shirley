import { useEffect, useRef, useState } from "react";
import { Download, LoaderCircle, Plus, RefreshCw, Trash2 } from "lucide-react";
import { agentBridge } from "@/lib/bridge";
import { emptyModel, emptyProvider, type ModelDefinition, type ProviderDefinition, type ProviderRegistry, type ProviderView, type ProviderProtocol } from "@/lib/model-providers";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";

const field = "mt-1 w-full rounded-control border border-line bg-canvas px-2.5 py-2 text-body-sm text-ink focus:outline-2 focus:outline-ink disabled:opacity-50";
const select = field;

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
      setScope(({ local: "本机", remote: "远端", demo: "演示模式：配置不会写入本机，也不会请求真实模型" })[bridge.capabilities().configurationScope]);
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
      setNotice("已保存。已有聊天保持当前连接；在模型列表中选择后应用，新聊天使用默认模型。");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const remove = async () => {
    if (!registry || !saved || !window.confirm(`删除“${saved.name}”？使用它的聊天将需要重新选择模型。`)) return;
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
      setNotice("已导入当前连接及默认模型。可以继续手动添加模型或从接口获取。");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const fetchModels = async () => {
    setBusy(true); setError(""); setNotice("");
    try {
      const entries = await (await agentBridge()).providers.fetchModels(inputProvider(), clearKey);
      setFetched(entries); setChosen(new Set());
      if (!entries.length) setNotice("接口未返回模型，请手动填写。");
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
      <p className="text-label text-muted">{scope} · 连接与密钥保存在当前执行端</p>
      <div className="grid gap-4 lg:grid-cols-[13rem_minmax(0,1fr)]">
        <aside className="flex flex-col gap-2">
          <Button size="sm" className="w-full" disabled={busy} onClick={() => edit(emptyProvider())}><Plus />新增服务商</Button>
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
                {registry.default_selection?.startsWith(`provider:${provider.id}:`) && <span className="text-caption text-muted">新聊天默认</span>}
              </button>
            ))}
          </div>
          <Button size="sm" variant="ghost" className="w-full" disabled={busy || !registry} onClick={() => void importLegacy()}>导入原有连接</Button>
        </aside>
        <form className="min-w-0 space-y-4" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="text-body-sm">服务商名称<input className={field} required disabled={busy} value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
            <label className="text-body-sm">接口协议<select className={select} disabled={busy} value={draft.protocol} onChange={(e) => setDraft({ ...draft, protocol: e.target.value as ProviderProtocol })}><option value="chat_completions">Chat Completions</option><option value="responses">Responses</option><option value="anthropic_messages">Anthropic Messages</option></select></label>
          </div>
          <label className="block text-body-sm">接口地址<input className={field} type="url" required disabled={busy} placeholder="https://api.example.com/v1" value={draft.base_url} onChange={(e) => setDraft({ ...draft, base_url: e.target.value })} /><span className="mt-1 block text-caption text-muted">支持服务根地址、/v1 或完整接口地址。</span></label>
          <label className="block text-body-sm">API Key<input className={field} type="password" autoComplete="new-password" disabled={busy || clearKey} placeholder={saved?.has_api_key && !routeChanged ? "已保存；留空保留原密钥" : "本地无认证服务可留空"} value={key} onChange={(e) => setKey(e.target.value)} /></label>
          {saved?.has_api_key && <label className="flex items-center gap-2 text-caption text-muted"><input type="checkbox" disabled={busy} checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />清除已保存的密钥</label>}
          {routeChanged && saved?.has_api_key && !key && !clearKey && <p className="text-caption text-muted">地址或协议已变更，旧密钥不会复用；需要认证时请重新填写。</p>}
          <details className="text-body-sm"><summary className="cursor-pointer text-muted">高级连接设置</summary><div className="mt-3 space-y-3">
            <label className="block">模型列表地址（可选）<input className={field} type="url" disabled={busy} placeholder="留空时自动推导 /models" value={draft.models_url || ""} onChange={(e) => setDraft({ ...draft, models_url: e.target.value || null })} /></label>
            <label className="block">额外请求参数（JSON 对象，可选）<textarea className={cn(field, "min-h-24 font-mono")} disabled={busy} placeholder='{"enable_thinking": false}' value={extraBody} onChange={(e) => setExtraBody(e.target.value)} /></label>
          </div></details>
          <section className="border-t border-line pt-3" aria-label="服务商模型列表">
            <div className="mb-3 flex flex-wrap items-center gap-2"><h3 className="mr-auto text-body-sm font-medium">模型列表</h3><Button size="sm" disabled={busy} onClick={() => setDraft((old) => ({ ...old, models: [...old.models, emptyModel()] }))}><Plus />手动添加</Button><Button size="sm" disabled={busy || !draft.base_url} onClick={() => void fetchModels()}><Download />从接口获取</Button></div>
            {fetched.length > 0 && <div className="mb-3 rounded-control border border-line p-3"><p className="mb-2 text-caption text-muted">选择要添加的模型。获取列表只检查列表接口，不代表已验证模型推理。</p><div className="max-h-44 overflow-auto">
              {fetched.map((model) => <label key={model.id} className="flex items-center gap-2 py-1 text-body-sm"><input type="checkbox" checked={chosen.has(model.id)} disabled={draft.models.some((old) => old.id === model.id)} onChange={(event) => setChosen((old) => { const next = new Set(old); if (event.target.checked) next.add(model.id); else next.delete(model.id); return next; })} /><span className="break-all">{model.name || model.id}</span></label>)}
            </div><Button size="sm" className="mt-2" disabled={!chosen.size} onClick={addFetched}>添加所选模型</Button></div>}
            <div className="space-y-3">{draft.models.map((model, index) => <div key={index} className="rounded-control border border-line p-3">
              <div className="flex items-start gap-2"><div className="grid min-w-0 flex-1 gap-2 sm:grid-cols-2"><label className="text-caption text-muted">模型 ID<input className={field} required disabled={busy} placeholder="实际请求使用的模型 ID" value={model.id} onChange={(e) => updateModel(index, { id: e.target.value })} /></label><label className="text-caption text-muted">显示名称<input className={field} disabled={busy} placeholder="留空时使用模型 ID" value={model.name} onChange={(e) => updateModel(index, { name: e.target.value })} /></label></div><button type="button" aria-label={`移除模型 ${model.id || index + 1}`} disabled={busy} className="mt-6 rounded-control p-2 text-muted hover:bg-selected hover:text-danger-ink" onClick={() => setDraft((old) => { const models = old.models.filter((_, i) => i !== index); return { ...old, models, default_model: old.default_model === model.id ? models[0]?.id || "" : old.default_model }; })}><Trash2 className="size-4" /></button></div>
              <details className="mt-2 text-caption text-muted"><summary className="cursor-pointer">模型能力 · 上下文 {model.context_window_tokens.toLocaleString()}{model.supports_images ? " · 图片" : ""}</summary><div className="mt-2 grid gap-3 sm:grid-cols-2">
                <label>上下文长度<input className={field} type="number" min="1" max="4294967295" step="1" required disabled={busy} value={model.context_window_tokens || ""} onChange={(e) => updateModel(index, { context_window_tokens: Number(e.target.value) })} /></label>
                <label>工具调用<select className={select} disabled={busy} value={model.tool_mode} onChange={(e) => updateModel(index, { tool_mode: e.target.value as ModelDefinition["tool_mode"] })}><option value="native">原生工具调用</option><option value="content">文本工具调用</option></select></label>
                <label className="flex items-center gap-2"><input type="checkbox" disabled={busy} checked={model.supports_images} onChange={(e) => updateModel(index, { supports_images: e.target.checked })} />支持图片</label>
                <label className="flex items-center gap-2"><input type="checkbox" disabled={busy} checked={model.thinking} onChange={(e) => updateModel(index, { thinking: e.target.checked })} />启用思考</label>
                <label>推理强度<select className={select} disabled={busy} value={model.reasoning_effort || ""} onChange={(e) => updateModel(index, { reasoning_effort: (e.target.value || null) as ModelDefinition["reasoning_effort"] })}><option value="">不指定</option><option value="low">低</option><option value="medium">中</option><option value="high">高</option></select></label>
              </div></details>
            </div>)}</div>
          </section>
          <label className="block text-body-sm">服务商默认模型<select className={select} required disabled={busy} value={draft.default_model} onChange={(e) => setDraft({ ...draft, default_model: e.target.value })}><option value="">请选择</option>{draft.models.filter((m) => m.id.trim()).map((model, i) => <option key={i} value={model.id}>{model.name || model.id}</option>)}</select></label>
          <label className="flex items-center gap-2 text-body-sm"><input type="checkbox" disabled={busy || currentDefault} checked={makeDefault} onChange={(e) => setMakeDefault(e.target.checked)} />{currentDefault ? "当前是新聊天默认；可在其他服务商中更改" : "设为新聊天的默认服务商和模型"}</label>
          {error && <p role="alert" className="break-words text-body-sm text-danger-ink">{error}</p>}{notice && <p role="status" className="text-body-sm text-muted">{notice}</p>}
          <footer className="flex flex-wrap items-center gap-2 border-t border-line pt-3"><Button type="submit" variant="primary" size="sm" disabled={busy || !registry}>{busy ? <LoaderCircle className="animate-spin" /> : null}保存配置</Button><Button size="sm" disabled={busy} onClick={() => void reload()}><RefreshCw />重新加载</Button>{saved && <Button size="sm" variant="danger" className="ml-auto" disabled={busy} onClick={() => void remove()}><Trash2 />删除服务商</Button>}</footer>
        </form>
      </div>
    </div>
  );
}
