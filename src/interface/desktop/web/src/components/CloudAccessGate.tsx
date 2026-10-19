import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/ui/card";

const fieldClass =
  "mt-2 w-full rounded-control border border-line bg-canvas px-3 py-2 text-body-sm text-ink focus:outline-2 focus:outline-ink";

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
        method: "POST",
        headers: { "content-type": "application/json", ...(value ? { authorization: `Bearer ${value}` } : {}) },
        body: "{}",
      });
      if (result.status === 401) {
        setRequired(true);
        if (value) setError("Incorrect access token");
        return;
      }
      if (!result.ok) throw new Error(`Connection failed (${result.status})`);
      sessionStorage.setItem("shirley.cloudToken", value);
      setReady(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    if (!bypass) void check(sessionStorage.getItem("shirley.cloudToken") || "");
  }, [bypass]);
  if (ready) return children;
  return (
    <main className="flex min-h-dvh items-center justify-center bg-canvas p-6 text-ink">
      <Card variant="elevated" className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Connect to Shirley Cloud</CardTitle>
          <CardDescription>Enter the access token to continue.</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              void check(token);
            }}
          >
            {required && (
              <label className="text-body-sm">
                Access token
                <input
                  className={fieldClass}
                  type="password"
                  autoComplete="current-password"
                  autoFocus
                  required
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                />
              </label>
            )}
            {error && (
              <p role="alert" className="text-body-sm text-danger-ink">
                {error}
              </p>
            )}
            <Button type="submit" disabled={busy}>
              {busy ? "Connecting…" : required ? "Connect" : "Reconnect"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
