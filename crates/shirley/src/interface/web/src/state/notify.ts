// Reply notifications, like a messaging app: when a bot finishes answering and you are not
// looking at that conversation, a system notification shows its first line; clicking it opens
// the conversation. Shown through the service worker, the only way Android Chrome allows.

import type { AgentEvent } from "../api/types";

const MAIN_SESSION = "main";
const PREVIEW_CHARS = 120;

let registration: Promise<ServiceWorkerRegistration | null> | null = null;

/** Register the service worker (installable app + notifications). Safe to call more than once. */
export function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (!registration) {
    registration =
      "serviceWorker" in navigator && window.isSecureContext
        ? navigator.serviceWorker.register("/sw.js").catch(() => null)
        : Promise.resolve(null);
  }
  return registration;
}

export function notificationsSupported(): boolean {
  return "Notification" in window && window.isSecureContext;
}

/**
 * Ask for permission once, from a user gesture (Chrome ignores requests without one). After the
 * user answers, the browser remembers the choice for this site; we never ask again.
 */
export function askNotificationPermission(): void {
  if (notificationsSupported() && Notification.permission === "default") {
    void Notification.requestPermission().catch(() => undefined);
  }
}

/** The plain text of a reply, for a notification body: no Markdown marks, one line. */
export function previewText(markdown: string): string {
  const text = markdown
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[#>*_`~|-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return text.length > PREVIEW_CHARS ? `${text.slice(0, PREVIEW_CHARS - 1)}…` : text;
}

/**
 * Collects each bot's reply as it streams and decides, when the turn ends, whether it is worth a
 * notification. Pure state, so it is testable; `show` does the side effect.
 */
export class ReplyWatcher {
  private replies = new Map<string, string>();

  /** Feed one event; returns the notification to show, if any. */
  take(event: AgentEvent, looking: (bot: string) => boolean): { bot: string; body: string } | null {
    if (!event.bot || (event.session ?? MAIN_SESSION) !== MAIN_SESSION) return null;
    const bot = event.bot;
    if (event.type === "content_delta") {
      this.replies.set(bot, (this.replies.get(bot) ?? "") + event.text);
      return null;
    }
    if (event.type === "error") {
      this.replies.delete(bot);
      return null;
    }
    if (event.type !== "finished") return null;
    const reply = this.replies.get(bot) ?? "";
    this.replies.delete(bot);
    // Stopped or interrupted by the user: they know, and there is nothing new to read.
    if (event.stop_reason === "cancelled") return null;
    if (looking(bot)) return null;
    return { bot, body: previewText(reply) || "回复好了" };
  }
}

/** Show one bot's notification; a newer one from the same bot replaces it. */
export async function show(bot: string, title: string, body: string): Promise<void> {
  if (!notificationsSupported() || Notification.permission !== "granted") return;
  const options: NotificationOptions & { renotify?: boolean } = {
    body,
    tag: `reply:${bot}`,
    renotify: true,
    icon: "/icons/icon-192.png",
    badge: "/icons/badge-96.png",
    data: { bot },
  };
  const worker = await registerServiceWorker();
  if (worker) {
    await worker.showNotification(title, options).catch(() => undefined);
    return;
  }
  try {
    const notification = new Notification(title, options);
    notification.onclick = () => {
      window.focus();
      window.dispatchEvent(new CustomEvent("shirley:open-bot", { detail: bot }));
      notification.close();
    };
  } catch {
    // Some browsers only allow worker notifications; without a worker there is nothing to do.
  }
}

/** The app icon's unread count (installed app on desktop Chrome), cleared when the app is seen. */
export function setBadge(count: number): void {
  const nav = navigator as Navigator & {
    setAppBadge?: (count?: number) => Promise<void>;
    clearAppBadge?: () => Promise<void>;
  };
  if (count > 0) void nav.setAppBadge?.(count).catch(() => undefined);
  else void nav.clearAppBadge?.().catch(() => undefined);
}
