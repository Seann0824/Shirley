import { useEffect, useState } from "react";
import { Cloud, HardDrive, LoaderCircle, RefreshCw } from "lucide-react";
import { Button } from "@/ui/button";
import { SettingsRow, SettingsSection } from "@/components/settings/controls";

type SyncStatus = {
  last_success_ms: number | null;
  error: string | null;
  uploaded: number;
  downloaded: number;
  conflicts: number;
  excluded: number;
};
type RemoteInfo = {
  url: string;
  has_token: boolean;
  sync_personal_memory: boolean;
  status: SyncStatus;
};
type MemoryReview = { id: string; revision: string; subject: string; body: string };

const native = "__TAURI_INTERNALS__" in window;
const remote = localStorage.getItem("shirley.connectionSource") === "remote";
const fieldClass = "mt-1 w-full rounded-control border border-line bg-canvas px-3 py-2 text-body-sm text-ink focus:outline-2 focus:outline-ink";

/**
 * Local / cloud connection configuration, embedded inside the unified Settings dialog.
 *
 * Extracted from the former standalone RemoteConnection modal. Behaviour is unchanged:
 * switch source (reloads the page), save endpoint + token, sync now, and review legacy
 * memories. In non-native (plain-browser debug) builds the connection settings are not
 * applicable, so it renders a short notice.
 */
export function ConnectionSettingsPanel({ canReload }: { canReload: boolean }) {
  const [info, setInfo] = useState<RemoteInfo | null>(null);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [sync, setSync] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [reviewRemote, setReviewRemote] = useState<boolean | null>(null);
  const [memories, setMemories] = useState<MemoryReview[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!native) return;
    void refresh(true).catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!native) {
    return <p className="text-body-sm text-muted">Connection settings are only available on the desktop / cloud runtime.</p>;
  }
  const refresh = async (populate = false) => {
    const { invoke } = await import("@tauri-apps/api/core");
    const next = await invoke<RemoteInfo>("agent_remote_info");
    setInfo(next);
    if (populate) { setUrl(next.url); setSync(next.sync_personal_memory); setToken(""); }
  };
  const switchSource = async (source: "local" | "remote") => {
    if (!canReload || busy) return;
    setBusy(true);
    setError("");
    try {
      if (source === "remote") {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("agent_remote_call", { command: "agent_list_projects", args: {} });
      }
      localStorage.setItem("shirley.connectionSource", source);
      window.location.reload();
    } catch (e) { setError(String(e)); setBusy(false); }
  };
  const save = async () => {
    setBusy(true);
    setError("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("agent_remote_save", { url, token: token || null, syncPersonalMemory: sync });
      setReviewRemote(null);
      setMemories([]);
      setSelected(new Set());
      if (remote) window.location.reload();
      else { await refresh(true); setBusy(false); }
    } catch (e) { setError(String(e)); setBusy(false); }
  };
  const syncNow = async () => {
    setBusy(true);
    setError("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("agent_remote_sync");
      await refresh();
    } catch (e) {
      setError(String(e));
      await refresh().catch(() => {});
    } finally { setBusy(false); }
  };
  const loadReview = async (cloud: boolean) => {
    setBusy(true);
    setError("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const entries = await invoke<MemoryReview[]>("agent_memory_review", { remote: cloud });
      setReviewRemote(cloud);
      setMemories(entries);
      setSelected(new Set());
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const confirmMemories = async () => {
    if (reviewRemote === null || selected.size === 0) return;
    setBusy(true);
    setError("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("agent_memory_confirm_personal", {
        remote: reviewRemote,
        approvals: memories.filter((entry) => selected.has(entry.id)).map(({ id, revision }) => ({ id, revision })),
      });
      await loadReview(reviewRemote);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  return (
    <div className="flex flex-col gap-2">
      <SettingsSection title="Execution location">
        <SettingsRow
          title="Current runtime"
          description="Once a remote chat is loaded, it keeps running in the remote project. Project memory stays on the machine that hosts the project."
        >
          <div className="flex gap-2">
            <Button size="sm" variant={!remote ? "primary" : "secondary"} disabled={busy || !canReload || !remote} onClick={() => void switchSource("local")}><HardDrive />Use local</Button>
            <Button size="sm" variant={remote ? "primary" : "secondary"} disabled={busy || !canReload || remote || !info?.url} onClick={() => void switchSource("remote")}><Cloud />Load remote</Button>
          </div>
        </SettingsRow>
      </SettingsSection>
      <p className="px-1 text-caption text-muted">Switching reloads the page; a running conversation continues on the original machine.{!canReload && " Send or clear the draft / queued messages first."}</p>

      <SettingsSection title="Cloud connection">
        <form className="flex flex-col gap-3 px-4 py-3.5" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <label className="text-body-sm">Cloud endpoint<input className={fieldClass} type="url" placeholder="https://shirley.example.com" value={url} onChange={(e) => setUrl(e.target.value)} disabled={busy} /></label>
          <label className="text-body-sm">Access token<input className={fieldClass} type="password" autoComplete="new-password" placeholder={info?.has_token ? "Saved; leave blank to keep the current token" : "Enter the cloud access token"} value={token} onChange={(e) => setToken(e.target.value)} disabled={busy} /></label>
          <label className="flex items-start gap-2 text-body-sm"><input className="mt-1" type="checkbox" checked={sync} onChange={(e) => setSync(e.target.checked)} disabled={busy} /><span>Share personal memory<span className="mt-1 block text-caption text-muted">Shared across projects and devices; a local copy is used when offline.</span></span></label>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" size="sm" disabled={busy || (remote && !canReload)}>{busy && <LoaderCircle className="animate-spin" />}{remote ? "Save and reload" : "Save and check connection"}</Button>
            <Button size="sm" onClick={() => void syncNow()} disabled={busy || !info?.url || !info.sync_personal_memory}><RefreshCw />Sync now</Button>
          </div>
        </form>
      </SettingsSection>

      <SettingsSection title="Legacy memory ownership">
        <div className="px-4 py-3.5">
          <p className="mb-2 text-caption text-muted">Legacy memories must be confirmed one by one; anything unconfirmed stays on the original machine.</p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={busy} onClick={() => void loadReview(false)}>Review local legacy memory</Button>
            <Button size="sm" disabled={busy || !info?.url} onClick={() => void loadReview(true)}>Review cloud legacy memory</Button>
          </div>
          {reviewRemote !== null && <div className="mt-3">
            <p className="mb-2 text-caption text-muted">{reviewRemote ? "Cloud" : "Local"} · {memories.length} pending</p>
            <div className="max-h-60 space-y-3 overflow-auto">
              {memories.map((entry) => <div key={entry.id} className="rounded-control border border-line p-2">
                <label className="flex items-start gap-2 text-body-sm"><input type="checkbox" disabled={busy} checked={selected.has(entry.id)} onChange={(event) => setSelected((old) => { const next = new Set(old); if (event.target.checked) next.add(entry.id); else next.delete(entry.id); return next; })} /><span className="break-words">{entry.subject}</span></label>
                <details className="mt-2 text-caption text-muted"><summary className="cursor-pointer">View full text</summary><p className="mt-2 whitespace-pre-wrap break-words text-ink">{entry.body}</p></details>
              </div>)}
            </div>
            {memories.length > 0 && <Button className="mt-3" size="sm" disabled={busy || selected.size === 0} onClick={() => void confirmMemories()}>Confirm selected as personal memory</Button>}
          </div>}
        </div>
      </SettingsSection>

      {info?.status.last_success_ms && <p className="px-1 text-caption text-muted">Last sync: {new Date(info.status.last_success_ms).toLocaleString()} · uploaded {info.status.uploaded} / downloaded {info.status.downloaded}</p>}
      {!!info?.status.excluded && <p className="px-1 text-caption text-muted">{info.status.excluded} memories were excluded (ownership unconfirmed or project-scoped).</p>}
      {(error || info?.status.error) && <p role="alert" className="px-1 break-words text-body-sm text-danger-ink">{error || info?.status.error}</p>}
    </div>
  );
}
