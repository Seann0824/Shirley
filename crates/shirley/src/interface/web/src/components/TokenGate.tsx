import { useEffect, useRef, useState } from "react";

import type { TokenError } from "../state/useAccessToken";

/** What each refusal means, in the words the person reading the screen needs. */
const MESSAGES: Record<Exclude<TokenError, null>, string> = {
  invalid: "令牌无效，请重新输入",
  unreachable: "无法连接服务器，请检查网络后重试",
  empty: "请输入访问令牌",
};

/**
 * The first-visit gate: no token, nothing loads, so this asks for one.
 *
 * It has no close button and ignores Esc on purpose: without a token the page can show
 * nothing, and dismissing the box would leave an empty screen with no way back.
 */
export function TokenGate({
  busy,
  error,
  onSubmit,
}: {
  busy: boolean;
  error: TokenError;
  onSubmit: (token: string) => void;
}) {
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    input.current?.focus();
  }, []);

  return (
    <div className="token-gate" role="dialog" aria-modal="true" aria-labelledby="token-gate-title">
      <div className="token-gate-backdrop" aria-hidden="true" />
      <form
        className="token-gate-card"
        onSubmit={(event) => {
          event.preventDefault();
          if (!busy) onSubmit(value);
        }}
      >
        <h2 id="token-gate-title">访问令牌</h2>
        <p className="token-gate-hint">
          这台服务器需要访问令牌才能使用。令牌在服务器 <code>/etc/shirley/cloud.env</code> 的{" "}
          <code>SHIRLEY_SERVER_TOKEN</code> 里，填一次就会记住。
        </p>
        <input
          ref={input}
          className="token-gate-input"
          type="password"
          value={value}
          onChange={(event) => setValue(event.target.value)}
          placeholder="粘贴访问令牌"
          aria-label="访问令牌"
          aria-invalid={error === "invalid" || error === "empty"}
          autoComplete="current-password"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          enterKeyHint="go"
        />
        {error && (
          <p className="token-gate-error" role="alert">
            {MESSAGES[error]}
          </p>
        )}
        <button className="token-gate-submit" type="submit" disabled={busy}>
          {busy ? "连接中…" : "连接"}
        </button>
      </form>
    </div>
  );
}
