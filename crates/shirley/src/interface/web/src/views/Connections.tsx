import { useCallback, useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, Connector } from "../api/types";
import { BackHeader, ErrorNote, LoadingRows } from "../components/Chrome";
import { Icon, type IconName } from "../components/Icon";

/** What each connection brings, in one line. */
const SUMMARY: Record<string, string> = {
  github: "提到你、分配给你、请你审查的 issue 和 PR，交给 bot 处理，并可直接评论回复",
  feishu: "私聊机器人或在群里 @ 它的消息，交给 bot 处理并在飞书里回复",
  slack: "@ 提及和私信，交给 bot 处理并在线程里回复",
  webhook: "任何能发 HTTP 请求的系统（CI、监控告警、脚本）都能把事情交给 bot",
};

const ICONS: Record<string, IconName> = {
  github: "code",
  feishu: "chat",
  slack: "chat",
  webhook: "plug",
};

function statusText(connector: Connector): string {
  if (!connector.connected) return "";
  switch (connector.status.state) {
    case "connected":
      return connector.status.events > 0 ? `已连接 · 处理了 ${connector.status.events} 件` : "已连接";
    case "connecting":
      return "连接中…";
    case "error":
      return "连接出错，自动重试中";
    default:
      return "已保存";
  }
}

/**
 * Connected services: where bots get work from besides this chat. Each connection hands its
 * events to one bot, which handles them in the background under its own role.
 */
export function Connections({ bots, onBack }: { bots: Bot[]; onBack: () => void }) {
  const [list, setList] = useState<Connector[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .connectors()
      .then((value) => {
        setList(value);
        setError(null);
      })
      .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
  }, []);

  useEffect(() => {
    load();
    // Status changes on its own (connecting -> connected, events counted).
    const timer = window.setInterval(load, 5000);
    return () => window.clearInterval(timer);
  }, [load]);

  return (
    <>
      <BackHeader title="连接" onBack={onBack} />
      {error && <ErrorNote message={error} />}
      {!list && !error && <LoadingRows />}
      {list && (
        <div className="connections-page">
          <p className="connections-intro">连上之后，那里发生的事会交给你选的 bot，按它的角色在后台处理，不占用你们的对话。</p>
          {list.map((connector) => (
            <article className={`connection-row ${connector.connected ? "on" : ""}`} key={connector.kind}>
              <header>
                <span className="connection-icon">
                  <Icon name={ICONS[connector.kind] ?? "plug"} size={18} />
                </span>
                <div>
                  <strong>
                    {connector.label}
                    {connector.connected && <em className={`connection-state ${connector.status.state}`}>{statusText(connector)}</em>}
                  </strong>
                  <p>{SUMMARY[connector.kind]}</p>
                  {connector.connected && connector.bot && (
                    <small>由 {bots.find((bot) => bot.name === connector.bot)?.display_name ?? connector.bot} 处理</small>
                  )}
                  {connector.status.state === "error" && connector.status.detail && (
                    <small className="connection-error">{connector.status.detail}</small>
                  )}
                </div>
                <button
                  className={`connection-action ${connector.connected ? "quiet" : ""}`}
                  onClick={() => setOpen((current) => (current === connector.kind ? null : connector.kind))}
                >
                  {open === connector.kind ? "收起" : connector.connected ? "管理" : "连接"}
                </button>
              </header>
              {open === connector.kind && (
                <ConnectForm
                  connector={connector}
                  bots={bots}
                  onDone={() => {
                    load();
                  }}
                  onClosed={() => {
                    setOpen(null);
                    load();
                  }}
                />
              )}
            </article>
          ))}
        </div>
      )}
    </>
  );
}

function ConnectForm({
  connector,
  bots,
  onDone,
  onClosed,
}: {
  connector: Connector;
  bots: Bot[];
  onDone: () => void;
  onClosed: () => void;
}) {
  const [values, setValues] = useState<Record<string, string>>(connector.values ?? {});
  const [bot, setBot] = useState(connector.bot ?? bots[0]?.name ?? "nono");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);

  const connect = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const { account } = await api.connect(connector.kind, bot, values);
      setMessage({ ok: true, text: account ? `已连接：${account}` : "已连接" });
      onDone();
    } catch (reason) {
      setMessage({ ok: false, text: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    try {
      await api.disconnect(connector.kind);
      onClosed();
    } catch (reason) {
      setMessage({ ok: false, text: reason instanceof Error ? reason.message : String(reason) });
      setBusy(false);
    }
  };

  const webhookUrl = connector.webhook_path ? `${window.location.origin}${connector.webhook_path}` : null;

  return (
    <div className="connect-form">
      {connector.fields.map((field) => (
        <label key={field.key}>
          <span>{field.label}</span>
          <input
            type={field.secret ? "password" : "text"}
            value={values[field.key] ?? ""}
            onChange={(event) => setValues((current) => ({ ...current, [field.key]: event.target.value }))}
            placeholder={field.secret && connector.connected ? "已保存，留空则不变" : ""}
            autoComplete="off"
          />
          <small>{field.help}</small>
        </label>
      ))}
      <label>
        <span>交给哪个 bot 处理</span>
        <select value={bot} onChange={(event) => setBot(event.target.value)}>
          {bots.map((item) => (
            <option key={item.name} value={item.name}>
              {item.display_name}
            </option>
          ))}
        </select>
      </label>
      {webhookUrl && (
        <div className="webhook-url">
          <span>把事件 POST 到这个地址（地址本身就是密钥，别公开）：</span>
          <code>{webhookUrl}</code>
          <button onClick={() => void navigator.clipboard?.writeText(webhookUrl)}>复制</button>
        </div>
      )}
      {message && <p className={message.ok ? "connect-ok" : "error-note"}>{message.text}</p>}
      <div className="connect-actions">
        <button className="connection-action" onClick={() => void connect()} disabled={busy}>
          {busy ? "检查中…" : connector.connected ? "保存" : "连接"}
        </button>
        {connector.connected && (
          <button className="connection-action quiet" onClick={() => void disconnect()} disabled={busy}>
            断开
          </button>
        )}
      </div>
    </div>
  );
}
