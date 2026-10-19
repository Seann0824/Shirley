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
    return <p className="text-body-sm text-muted">连接设置仅在桌面 / 云端执行端可用。</p>;
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
      <SettingsSection title="执行位置">
        <SettingsRow
          title="当前执行端"
          description="远端聊天加载后，继续在远端项目中执行。项目记忆留在项目所在机器。"
        >
          <div className="flex gap-2">
            <Button size="sm" variant={!remote ? "primary" : "secondary"} disabled={busy || !canReload || !remote} onClick={() => void switchSource("local")}><HardDrive />使用本地</Button>
            <Button size="sm" variant={remote ? "primary" : "secondary"} disabled={busy || !canReload || remote || !info?.url} onClick={() => void switchSource("remote")}><Cloud />加载远端</Button>
          </div>
        </SettingsRow>
      </SettingsSection>
      <p className="px-1 text-caption text-muted">切换会重新加载页面；正在执行的对话会在原机器继续。{!canReload && "请先发送或清空草稿、待发消息。"}</p>

      <SettingsSection title="云端连接">
        <form className="flex flex-col gap-3 px-4 py-3.5" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <label className="text-body-sm">云端地址<input className={fieldClass} type="url" placeholder="https://shirley.example.com" value={url} onChange={(e) => setUrl(e.target.value)} disabled={busy} /></label>
          <label className="text-body-sm">访问令牌<input className={fieldClass} type="password" autoComplete="new-password" placeholder={info?.has_token ? "已保存；留空保留原令牌" : "填写云端访问令牌"} value={token} onChange={(e) => setToken(e.target.value)} disabled={busy} /></label>
          <label className="flex items-start gap-2 text-body-sm"><input className="mt-1" type="checkbox" checked={sync} onChange={(e) => setSync(e.target.checked)} disabled={busy} /><span>共享个人记忆<span className="mt-1 block text-caption text-muted">跨项目、跨设备共享；断网时使用本地副本。</span></span></label>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" size="sm" disabled={busy || (remote && !canReload)}>{busy && <LoaderCircle className="animate-spin" />}{remote ? "保存并重新加载" : "保存并检查连接"}</Button>
            <Button size="sm" onClick={() => void syncNow()} disabled={busy || !info?.url || !info.sync_personal_memory}><RefreshCw />立即同步</Button>
          </div>
        </form>
      </SettingsSection>

      <SettingsSection title="旧记忆归属">
        <div className="px-4 py-3.5">
          <p className="mb-2 text-caption text-muted">旧记忆需逐条确认归属，未确认的内容留在原机器。</p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={busy} onClick={() => void loadReview(false)}>审查本地旧记忆</Button>
            <Button size="sm" disabled={busy || !info?.url} onClick={() => void loadReview(true)}>审查云端旧记忆</Button>
          </div>
          {reviewRemote !== null && <div className="mt-3">
            <p className="mb-2 text-caption text-muted">{reviewRemote ? "云端" : "本地"} · {memories.length} 条待确认</p>
            <div className="max-h-60 space-y-3 overflow-auto">
              {memories.map((entry) => <div key={entry.id} className="rounded-control border border-line p-2">
                <label className="flex items-start gap-2 text-body-sm"><input type="checkbox" disabled={busy} checked={selected.has(entry.id)} onChange={(event) => setSelected((old) => { const next = new Set(old); if (event.target.checked) next.add(entry.id); else next.delete(entry.id); return next; })} /><span className="break-words">{entry.subject}</span></label>
                <details className="mt-2 text-caption text-muted"><summary className="cursor-pointer">查看全文</summary><p className="mt-2 whitespace-pre-wrap break-words text-ink">{entry.body}</p></details>
              </div>)}
            </div>
            {memories.length > 0 && <Button className="mt-3" size="sm" disabled={busy || selected.size === 0} onClick={() => void confirmMemories()}>确认所选为个人记忆</Button>}
          </div>}
        </div>
      </SettingsSection>

      {info?.status.last_success_ms && <p className="px-1 text-caption text-muted">上次同步：{new Date(info.status.last_success_ms).toLocaleString()} · 上传 {info.status.uploaded} / 下载 {info.status.downloaded}</p>}
      {!!info?.status.excluded && <p className="px-1 text-caption text-muted">{info.status.excluded} 条记忆归属未确认或属于项目，未参与同步。</p>}
      {!!info?.status.conflicts && <p className="px-1 text-caption text-danger-ink">{info.status.conflicts} 项冲突待处理，双方版本均已保留。</p>}
      {(error || info?.status.error) && <p role="alert" className="px-1 break-words text-body-sm text-danger-ink">{error || info?.status.error}</p>}
    </div>
  );
}
