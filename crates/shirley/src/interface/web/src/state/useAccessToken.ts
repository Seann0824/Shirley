import { useCallback, useEffect, useState } from "react";

import { adoptToken, currentToken, needsToken, onUnauthorized, verifyToken } from "../api/token";

export type TokenError = "invalid" | "unreachable" | "empty" | null;

/**
 * Whether the interface has to ask for the access token, and how to answer.
 *
 * The gate opens when the page has no token at all, and again whenever the server
 * refuses one (401/403), because a token can expire or be mistyped on another device.
 */
export function useAccessToken() {
  const [needed, setNeeded] = useState(() => needsToken(currentToken()));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<TokenError>(null);

  useEffect(() => onUnauthorized(() => setNeeded(true)), []);

  /** Check the token with the server; on success the page reloads with it in place. */
  const submit = useCallback(async (candidate: string): Promise<boolean> => {
    const token = candidate.trim();
    if (!token) {
      setError("empty");
      return false;
    }
    setBusy(true);
    setError(null);
    const verdict = await verifyToken(token, fetch);
    if (verdict !== "ok") {
      setBusy(false);
      setError(verdict === "invalid" ? "invalid" : "unreachable");
      return false;
    }
    adoptToken(token);
    // A reload, not a patch: the event stream and every cached read restart with this token.
    window.location.reload();
    return true;
  }, []);

  return { needed, busy, error, submit };
}
