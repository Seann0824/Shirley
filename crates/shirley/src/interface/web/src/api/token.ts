// The cloud access token: where it comes from, where it is kept, and whether the
// interface has to ask for one before it can talk to the server.
//
// The pure half (search string <-> token, storage <-> token, the probe) is what the
// tests drive; the small mutable half below it is the one value the bridge reads.

export const TOKEN_KEY = "shirley.token";

/** The command the gate probes: anything that needs the token, cheap and side-effect free. */
export const PROBE_COMMAND = "agent_list_bots";

/** Only the two halves of `Storage` this needs, so tests can pass a plain object. */
export interface TokenStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function clean(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

/** A share link carries the token once, as `?token=…`. */
export function tokenFromLink(search: string): string | null {
  return clean(new URLSearchParams(search).get("token"));
}

/** The same search string without `token=`, so it never sits in the address bar. */
export function withoutTokenLink(search: string): string {
  const params = new URLSearchParams(search);
  if (!params.has("token")) return search;
  params.delete("token");
  const rest = params.toString();
  return rest ? `?${rest}` : "";
}

/** The link wins over storage: it is the newer handoff. A link token is kept. */
export function readToken(storage: TokenStorage | null, search: string): string | null {
  const fromLink = tokenFromLink(search);
  if (fromLink) {
    saveToken(storage, fromLink);
    return fromLink;
  }
  try {
    return clean(storage?.getItem(TOKEN_KEY));
  } catch {
    return null;
  }
}

export function saveToken(storage: TokenStorage | null, token: string): void {
  try {
    storage?.setItem(TOKEN_KEY, token.trim());
  } catch {
    // Private mode: the token still works for this page, it just is not remembered.
  }
}

/** No usable token at all: ask before showing anything that needs one. */
export function needsToken(token: string | null | undefined): boolean {
  return clean(token) === null;
}

export type TokenCheck = "ok" | "invalid" | "unreachable";

/**
 * Ask the server whether `token` is good: anything but 2xx, 401 and 403 is told apart,
 * because "wrong token" and "cannot reach the server" want different words on screen.
 */
export async function verifyToken(
  token: string,
  fetchImpl: typeof fetch,
  command: string = PROBE_COMMAND,
): Promise<TokenCheck> {
  try {
    const response = await fetchImpl(`/api/cmd/${command}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${token.trim()}` },
      body: "{}",
    });
    // A rejection body is irrelevant: the status alone says what went wrong.
    void response.body?.cancel().catch(() => {});
    if (response.ok) return "ok";
    return response.status === 401 || response.status === 403 ? "invalid" : "unreachable";
  } catch {
    return "unreachable";
  }
}

// ---------------------------------------------------------------------------
// The live token: one value, read by the bridge on every request.
// ---------------------------------------------------------------------------

function browserStorage(): TokenStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

let current: string | null = null;
let loaded = false;

/** Read once (link first, then storage) and drop `?token=` from the URL. */
export function currentToken(): string | null {
  if (loaded) return current;
  loaded = true;
  if (typeof window === "undefined") return current;
  current = readToken(browserStorage(), window.location.search);
  stripTokenFromUrl();
  return current;
}

export function stripTokenFromUrl(): void {
  if (typeof window === "undefined" || !window.history?.replaceState) return;
  const search = withoutTokenLink(window.location.search);
  if (search === window.location.search) return;
  const { pathname, hash } = window.location;
  try {
    window.history.replaceState(window.history.state, "", `${pathname}${search}${hash}`);
  } catch {
    // Nothing to repair: the page works either way.
  }
}

/**
 * Take a token the server just accepted: remember it, clear it from the address bar,
 * and make it the one the bridge sends. The caller reloads so every request and the
 * event stream start from a clean state.
 */
export function adoptToken(value: string): void {
  current = clean(value);
  loaded = true;
  if (current) saveToken(browserStorage(), current);
  stripTokenFromUrl();
}

// ---------------------------------------------------------------------------
// "The server refused us": any 401/403 raises the gate again, expired or mistyped.
// ---------------------------------------------------------------------------

const unauthorizedListeners = new Set<() => void>();

export function onUnauthorized(listener: () => void): () => void {
  unauthorizedListeners.add(listener);
  return () => {
    unauthorizedListeners.delete(listener);
  };
}

export function reportUnauthorized(): void {
  unauthorizedListeners.forEach((listener) => listener());
}
