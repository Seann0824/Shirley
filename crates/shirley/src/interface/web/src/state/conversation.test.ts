import { describe, expect, it } from "vitest";

import type { ItemWire } from "../api/types";
import { fromSnapshot, reduce, type ConversationItem } from "./conversation";

const done: ItemWire = {
  kind: "tools",
  calls: [{ id: "c1", name: "bash", arguments: "{}", output: "ok", ok: null, parts: [] }],
};

describe("fromSnapshot", () => {
  it("settles restored tool groups so old turns stop spinning", () => {
    const items = fromSnapshot([done], false);
    expect(items[0]).toMatchObject({ kind: "tools", settled: true });
  });

  it("keeps the newest group of a turn that is still running live", () => {
    const items = fromSnapshot([{ kind: "message", role: "user", content: "hi", parts: [] }, done], true);
    expect(items[1]).toMatchObject({ kind: "tools", settled: false });
  });

  it("only the newest group stays live, older ones are settled", () => {
    const items = fromSnapshot([done, done], true);
    expect(items[0]).toMatchObject({ settled: true });
    expect(items[1]).toMatchObject({ settled: false });
  });

  it("copies the unchanged message items through", () => {
    const items = fromSnapshot([{ kind: "message", role: "user", content: "hi", parts: [] }], true);
    expect(items).toEqual([{ kind: "message", role: "user", content: "hi", parts: [] }]);
  });
});

describe("reduce", () => {
  it("groups adjacent live calls and settles them as they finish", () => {
    let items: ConversationItem[] = [];
    items = reduce(items, { type: "tool_started", call_id: "a", name: "bash", arguments: "{}" });
    items = reduce(items, { type: "tool_started", call_id: "b", name: "grep", arguments: "{}" });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "tools" });
    // A live group carries no `settled` flag: its calls animate until they finish.
    expect(items[0]).not.toHaveProperty("settled");

    items = reduce(items, {
      type: "tool_finished",
      call_id: "a",
      name: "bash",
      ok: true,
      output: "done",
      parts: [],
      elapsed_ms: 5,
    });
    const group = items[0];
    expect(group.kind).toBe("tools");
    if (group.kind === "tools") {
      expect(group.calls.map((call) => call.ok)).toEqual([true, null]);
    }
  });

  it("seals a streaming reply when a tool starts, keeping the order", () => {
    let items: ConversationItem[] = [];
    items = reduce(items, { type: "content_delta", text: "先看看仓库" });
    items = reduce(items, { type: "tool_started", call_id: "a", name: "bash", arguments: "{}" });
    expect(items.map((item) => item.kind)).toEqual(["message", "tools"]);
    expect(items[0]).toMatchObject({ streaming: false });
  });
});
