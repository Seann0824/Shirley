import { describe, expect, it } from "vitest";

import { cardsByTurnEnd, type ConversationItem } from "./conversation";

const user = (content: string) => ({ kind: "message", role: "user", content, parts: [] }) as ConversationItem;
const bot = (content: string) => ({ kind: "message", role: "assistant", content, parts: [] }) as ConversationItem;
const tools = (...names: string[]) =>
  ({
    kind: "tools",
    calls: names.map((name, i) => ({ id: `${name}-${i}`, name, arguments: "{}", output: null })),
    settled: true,
  }) as unknown as ConversationItem;

describe("cardsByTurnEnd", () => {
  it("puts a turn's task cards after its last item", () => {
    const items = [user("做个页面"), tools("dispatch_task"), bot("好，我安排去做了"), user("谢谢"), bot("不客气")];
    const cards = cardsByTurnEnd(items);
    expect([...cards.keys()]).toEqual([2]);
    expect(cards.get(2)?.map((call) => call.name)).toEqual(["dispatch_task"]);
  });

  it("keeps cards of the latest turn at the end, and ignores other tools", () => {
    const items = [user("a"), tools("read", "dispatch_task"), tools("dispatch_task")];
    const cards = cardsByTurnEnd(items);
    expect([...cards.keys()]).toEqual([2]);
    expect(cards.get(2)).toHaveLength(2);
    expect(cardsByTurnEnd([user("hi"), bot("hello")]).size).toBe(0);
  });
});
