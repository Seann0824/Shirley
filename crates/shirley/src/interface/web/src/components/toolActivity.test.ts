import { describe, expect, it } from "vitest";

import type { ToolCallWire } from "../api/types";
import { showsToolActivity, toolActivity } from "./toolActivity";

function call(over: Partial<ToolCallWire> = {}): ToolCallWire {
  return { id: "c1", name: "bash", arguments: "{}", output: null, ok: null, parts: [], ...over };
}

describe("toolActivity", () => {
  it("animates while a live call has no outcome", () => {
    expect(toolActivity([call()], false).running).toBe(true);
  });

  it("never animates a group restored from the log, whose outcome is unknown", () => {
    // History comes back with `ok: None`, which is not the same thing as still running.
    expect(toolActivity([call()], true).running).toBe(false);
  });

  it("stops animating once every live call has reported", () => {
    expect(toolActivity([call({ ok: true })], false).running).toBe(false);
  });

  it("still animates when one sibling is done and another is not", () => {
    expect(toolActivity([call({ id: "a", ok: false }), call({ id: "b" })], false).running).toBe(true);
  });

  it("keeps produced files but never the text of a call", () => {
    const activity = toolActivity(
      [
        call({
          ok: true,
          parts: [
            { kind: "text", text: "noise" },
            { kind: "image", source: { kind: "artifact", id: "a1" }, alt: "chart" },
          ],
        }),
      ],
      true,
    );
    expect(activity.running).toBe(false);
    expect(activity.parts).toEqual([{ kind: "image", source: { kind: "artifact", id: "a1" }, alt: "chart" }]);
  });

  it("shows nothing for a settled group that produced nothing", () => {
    expect(showsToolActivity(toolActivity([call({ ok: true })], true))).toBe(false);
  });

  it("shows the row when there is activity or an artifact", () => {
    expect(showsToolActivity(toolActivity([call()], false))).toBe(true);
    expect(
      showsToolActivity(
        toolActivity([call({ ok: true, parts: [{ kind: "file", path: "a.csv", name: "a.csv" }] })], true),
      ),
    ).toBe(true);
  });

  it("shows nothing at all for an empty group", () => {
    expect(showsToolActivity(toolActivity([], true))).toBe(false);
  });
});
