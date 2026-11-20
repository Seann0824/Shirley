import { describe, expect, it } from "vitest";

import type { AgentEvent, Bot } from "../api/types";
import { markIdle, markReplying, noteEvent, replyingFromList, replyingSnapshot } from "./replying";

function bot(name: string, running?: boolean): Bot {
  return { name, display_name: name, tools: [], skills: [], persona: null, running };
}

async function current(): Promise<Set<string>> {
  return replyingSnapshot();
}

describe("replying", () => {
  it("starts from the server's list", async () => {
    replyingFromList([bot("a", true), bot("b", false), bot("c")]);
    expect([...(await current())]).toEqual(["a"]);
  });

  it("follows the event stream of main sessions only", async () => {
    replyingFromList([]);
    noteEvent({ type: "content_delta", text: "hi", bot: "a" } as AgentEvent);
    noteEvent({ type: "tool_started", call_id: "1", name: "bash", arguments: "", bot: "b", session: "task-1" } as AgentEvent);
    expect([...(await current())]).toEqual(["a"]);
    noteEvent({ type: "finished", stop_reason: "completed", bot: "a" } as AgentEvent);
    expect((await current()).size).toBe(0);
  });

  it("is marked by a send and cleared by the server's word", async () => {
    replyingFromList([]);
    markReplying("a");
    expect((await current()).has("a")).toBe(true);
    markIdle("a");
    expect((await current()).has("a")).toBe(false);
  });
});
