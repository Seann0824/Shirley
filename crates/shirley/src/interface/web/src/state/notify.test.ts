import { describe, expect, it } from "vitest";

import type { AgentEvent } from "../api/types";
import { ReplyWatcher, previewText } from "./notify";

const ev = (event: Record<string, unknown>) => ({ session: "main", ...event }) as unknown as AgentEvent;

describe("ReplyWatcher", () => {
  it("notifies with the reply when the conversation is not in view", () => {
    const watcher = new ReplyWatcher();
    const away = () => false;
    expect(watcher.take(ev({ bot: "nono", type: "content_delta", text: "## 好了\n\n**结果**：" }), away)).toBeNull();
    expect(watcher.take(ev({ bot: "nono", type: "content_delta", text: "三项都完成。" }), away)).toBeNull();
    expect(watcher.take(ev({ bot: "nono", type: "finished", stop_reason: "completed" }), away)).toEqual({
      bot: "nono",
      body: "好了 结果 ：三项都完成。",
    });
  });

  it("stays quiet when looking, when stopped, and for task sessions", () => {
    const watcher = new ReplyWatcher();
    expect(watcher.take(ev({ bot: "nono", type: "finished", stop_reason: "completed" }), () => true)).toBeNull();
    expect(watcher.take(ev({ bot: "nono", type: "finished", stop_reason: "cancelled" }), () => false)).toBeNull();
    expect(
      watcher.take(ev({ bot: "nono", session: "task-1", type: "finished", stop_reason: "completed" }), () => false),
    ).toBeNull();
  });

  it("keeps each bot's reply apart and starts fresh after each turn", () => {
    const watcher = new ReplyWatcher();
    const away = () => false;
    watcher.take(ev({ bot: "nono", type: "content_delta", text: "A" }), away);
    watcher.take(ev({ bot: "scout", type: "content_delta", text: "B" }), away);
    expect(watcher.take(ev({ bot: "scout", type: "finished", stop_reason: "completed" }), away)?.body).toBe("B");
    expect(watcher.take(ev({ bot: "nono", type: "finished", stop_reason: "completed" }), away)?.body).toBe("A");
    expect(watcher.take(ev({ bot: "nono", type: "finished", stop_reason: "completed" }), away)?.body).toBe("回复好了");
  });
});

describe("previewText", () => {
  it("strips markdown and shortens", () => {
    expect(previewText("看 [文档](https://x.y) 和\n```js\ncode\n```\n完成")).toBe("看 文档 和 完成");
    expect(previewText("字".repeat(300)).length).toBe(120);
  });
});
