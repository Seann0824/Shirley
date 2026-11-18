import { describe, expect, it, vi } from "vitest";

import {
  TOKEN_KEY,
  adoptToken,
  currentToken,
  needsToken,
  onUnauthorized,
  readToken,
  reportUnauthorized,
  saveToken,
  tokenFromLink,
  verifyToken,
  withoutTokenLink,
  type TokenStorage,
} from "./token";

/** A `localStorage` stand-in: the real one does not exist under vitest's node environment. */
function store(initial: Record<string, string> = {}, options: { throws?: boolean } = {}) {
  const data = new Map(Object.entries(initial));
  const storage: TokenStorage = {
    getItem: (key) => {
      if (options.throws) throw new Error("denied");
      return data.get(key) ?? null;
    },
    setItem: (key, value) => {
      if (options.throws) throw new Error("denied");
      data.set(key, value);
    },
  };
  return { storage, data };
}

function respond(status: number) {
  return vi.fn(async () => new Response(null, { status })) as unknown as typeof fetch;
}

describe("tokenFromLink", () => {
  it("reads ?token= and ignores the rest of the query", () => {
    expect(tokenFromLink("?token=abc123")).toBe("abc123");
    expect(tokenFromLink("?bot=nono&token=abc123&x=1")).toBe("abc123");
    // A pasted token often arrives with stray spaces: they are not part of it.
    expect(tokenFromLink("?token=%20abc123%20")).toBe("abc123");
  });

  it("treats a missing or blank token as absent", () => {
    expect(tokenFromLink("")).toBeNull();
    expect(tokenFromLink("?bot=nono")).toBeNull();
    expect(tokenFromLink("?token=")).toBeNull();
    expect(tokenFromLink("?token=%20")).toBeNull();
  });
});

describe("withoutTokenLink", () => {
  it("drops the token but keeps every other parameter", () => {
    expect(withoutTokenLink("?token=abc&bot=nono")).toBe("?bot=nono");
    expect(withoutTokenLink("?bot=nono&token=abc")).toBe("?bot=nono");
  });

  it("returns the search unchanged when there is no token to hide", () => {
    expect(withoutTokenLink("")).toBe("");
    expect(withoutTokenLink("?bot=nono")).toBe("?bot=nono");
  });

  it("leaves no trailing question mark behind", () => {
    expect(withoutTokenLink("?token=abc")).toBe("");
  });
});

describe("readToken", () => {
  it("keeps a token that arrives in the link", () => {
    const { storage, data } = store();
    expect(readToken(storage, "?token=from-link")).toBe("from-link");
    expect(data.get(TOKEN_KEY)).toBe("from-link");
  });

  it("prefers the link over a stored token: it is the newer handoff", () => {
    const { storage } = store({ [TOKEN_KEY]: "stored" });
    expect(readToken(storage, "?token=fresh")).toBe("fresh");
  });

  it("falls back to what the device remembered", () => {
    const { storage } = store({ [TOKEN_KEY]: "  stored  " });
    expect(readToken(storage, "")).toBe("stored");
  });

  it("says nothing when there is nothing to read", () => {
    const { storage } = store();
    expect(readToken(storage, "")).toBeNull();
    expect(readToken(null, "")).toBeNull();
    expect(readToken(store({}, { throws: true }).storage, "")).toBeNull();
  });
});

describe("saveToken / needsToken", () => {
  it("stores a trimmed token and survives storage that refuses", () => {
    const { storage, data } = store();
    saveToken(storage, "  abc  ");
    expect(data.get(TOKEN_KEY)).toBe("abc");
    expect(() => saveToken(store({}, { throws: true }).storage, "abc")).not.toThrow();
    expect(() => saveToken(null, "abc")).not.toThrow();
  });

  it("only asks when there is no usable token", () => {
    expect(needsToken(null)).toBe(true);
    expect(needsToken("")).toBe(true);
    expect(needsToken("   ")).toBe(true);
    expect(needsToken("abc")).toBe(false);
  });
});

describe("verifyToken", () => {
  it("posts to an authenticated command with the bearer token", async () => {
    const fetchImpl = respond(200);
    expect(await verifyToken("  abc  ", fetchImpl)).toBe("ok");
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [url, init] = (fetchImpl as unknown as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(url).toBe("/api/cmd/agent_list_bots");
    expect(init.method).toBe("POST");
    expect(init.headers.Authorization).toBe("Bearer abc");
  });

  it("calls a refused token invalid, not a server problem", async () => {
    expect(await verifyToken("expired", respond(401))).toBe("invalid");
    expect(await verifyToken("expired", respond(403))).toBe("invalid");
  });

  it("calls a broken server unreachable", async () => {
    expect(await verifyToken("abc", respond(500))).toBe("unreachable");
    expect(await verifyToken("abc", respond(502))).toBe("unreachable");
  });

  it("calls a failed request unreachable", async () => {
    const offline = vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    }) as unknown as typeof fetch;
    expect(await verifyToken("abc", offline)).toBe("unreachable");
  });
});

describe("adoptToken against the live value", () => {
  it("has no token to offer before anything is read", () => {
    // No `window` under vitest: the module reports what it was handed and nothing else.
    expect(currentToken()).toBeNull();
  });

  it("lets a checked token become the one the bridge sends", () => {
    adoptToken("  live-token  ");
    expect(currentToken()).toBe("live-token");
  });
});

describe("onUnauthorized", () => {
  it("tells every listener and can be unsubscribed", () => {
    const one = vi.fn();
    const two = vi.fn();
    const off = onUnauthorized(one);
    const offTwo = onUnauthorized(two);
    reportUnauthorized();
    expect(one).toHaveBeenCalledTimes(1);
    expect(two).toHaveBeenCalledTimes(1);
    off();
    reportUnauthorized();
    expect(one).toHaveBeenCalledTimes(1);
    expect(two).toHaveBeenCalledTimes(2);
    offTwo();
  });
});
