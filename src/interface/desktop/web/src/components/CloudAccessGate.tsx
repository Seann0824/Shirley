import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/ui/button";

/** Same-origin cloud login. Credentials are held only for the current browser session. */
export function CloudAccessGate({ children }: { children: ReactNode }) {
  const bypass = "__TAURI_INTERNALS__" in window || import.meta.env.DEV;
  const [ready, setReady] = useState(bypass);
  const [required, setRequired] = useState(false);
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const check = async (value: string) => {
    setBusy(true);
    setError("");
    try {
      const result = await fetch("/api/cmd/agent_list_projects", {
        signal: AbortSignal.timeout(10_000),
        method: "POST", headers: { "content-type": "application/json", ...(value ? { authorization: `Bearer ${value}` } : {}) }, body: "{}",
      });
      if (result.status === 401) { setRequired(true); if (value) setError("访问令牌不正确"); return; }
      if (!result.ok) throw new Error(`连接失败 (${result.status})`);
      sessionStorage.setItem("shirley.cloudToken", value);
      setReady(true);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  useEffect(() => { if (!bypass) void check(sessionStorage.getItem("shirley.cloudToken") || ""); }, [bypass]);
  if (ready) return children;
  return <main className="flex min-h-dvh items-center justify-center bg-canvas p-6 text-ink">
    <form className="flex w-full max-w-sm flex-col gap-4 rounded-card border border-line p-6" onSubmit={(e) => { e.preventDefault(); void check(token); }}>
      <h1 className="text-body font-medium">连接 Shirley 云端</h1>
      {required && <label className="text-body-sm">访问令牌<input type="password" autoComplete="current-password" autoFocus required value={token} onChange={(e) => setToken(e.target.value)} className="mt-2 w-full rounded-control border border-line bg-canvas px-3 py-2 text-ink" /></label>}
      {error && <p role="alert" className="text-body-sm text-danger-ink">{error}</p>}
      <Button type="submit" disabled={busy}>{busy ? "连接中…" : required ? "连接" : "重新连接"}</Button>
    </form>
  </main>;
}
