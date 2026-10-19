export type ProviderProtocol = "chat_completions" | "responses" | "anthropic_messages";
export type ModelDefinition = {
  id: string;
  name: string;
  context_window_tokens: number;
  supports_images: boolean;
  tool_mode: "native" | "content";
  thinking: boolean;
  reasoning_effort: "low" | "medium" | "high" | null;
};
export type ProviderDefinition = {
  id: string;
  name: string;
  protocol: ProviderProtocol;
  base_url: string;
  models_url: string | null;
  api_key: string | null;
  default_model: string;
  models: ModelDefinition[];
  extra_body: Record<string, unknown> | null;
};
export type ProviderView = ProviderDefinition & { has_api_key: boolean };
export type ProviderRegistry = { revision: string; providers: ProviderView[]; default_selection: string | null };
export type SaveProvider = { revision: string; provider: ProviderDefinition; clear_api_key: boolean; set_default: boolean };
export type ProviderBridge = {
  get: () => Promise<ProviderRegistry>;
  save: (input: SaveProvider) => Promise<ProviderRegistry>;
  delete: (revision: string, id: string) => Promise<ProviderRegistry>;
  importLegacy: (revision: string) => Promise<ProviderRegistry>;
  fetchModels: (provider: ProviderDefinition, clear_api_key: boolean) => Promise<ModelDefinition[]>;
};
export const selectionKey = (provider: string, model: string) => `provider:${provider}:${encodeURIComponent(model).replace(/%20/g, "+").replace(/[!'()~]/g, (ch) => `%${ch.charCodeAt(0).toString(16).toUpperCase()}`)}`;
export function emptyModel(): ModelDefinition {
  return { id: "", name: "", context_window_tokens: 65536, supports_images: false, tool_mode: "native", thinking: false, reasoning_effort: null };
}
export function emptyProvider(): ProviderDefinition {
  return { id: globalThis.crypto?.randomUUID?.() ?? `provider-${Date.now()}-${Math.random().toString(36).slice(2)}`, name: "New provider", protocol: "chat_completions", base_url: "", models_url: null, api_key: null, default_model: "", models: [emptyModel()], extra_body: null };
}
export function providerBridge(call: <T>(command: string, args?: Record<string, unknown>) => Promise<T>): ProviderBridge {
  const action = <T>(action: string, input: unknown = {}) => call<T>("agent_model_providers", { action, input });
  return {
    get: () => action("get"),
    save: (input) => action("save", input),
    delete: (revision, id) => action("delete", { revision, id }),
    importLegacy: (revision) => action("import", { revision }),
    fetchModels: (provider, clear_api_key) => action("fetch", { provider, clear_api_key }),
  };
}

/** Standalone frontend preview: no real endpoints or keys are written. */
export function mockProviderBridge(): ProviderBridge {
  let state: ProviderRegistry = { revision: "0", providers: [], default_selection: null };
  const snapshot = () => structuredClone(state);
  const check = (revision: string) => { if (state.revision !== revision) throw new Error("Model config changed; please reload"); };
  return {
    async get() { return snapshot(); },
    async save(input) {
      check(input.revision);
      const has_api_key = !input.clear_api_key && (!!input.provider.api_key || !!state.providers.find((p) => p.id === input.provider.id)?.has_api_key);
      state.providers = [...state.providers.filter((p) => p.id !== input.provider.id), { ...structuredClone(input.provider), api_key: null, has_api_key }];
      if (input.set_default || !state.default_selection) state.default_selection = selectionKey(input.provider.id, input.provider.default_model);
      state.revision = String(Number(state.revision) + 1);
      return snapshot();
    },
    async delete(revision, id) {
      check(revision); state.providers = state.providers.filter((p) => p.id !== id); state.revision = String(Number(state.revision) + 1);
      if (state.default_selection?.startsWith(`provider:${id}:`)) state.default_selection = null;
      return snapshot();
    },
    async importLegacy() { throw new Error("Demo mode has no existing connection to import"); },
    async fetchModels() { throw new Error("Demo mode does not call real model endpoints; add models manually"); },
  };
}
