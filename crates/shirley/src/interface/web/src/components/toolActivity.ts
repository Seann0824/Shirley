import type { RenderPart, ToolCallWire } from "../api/types";

/** What a tool group should show: whether it is still working, and any files it produced. */
export interface ToolActivity {
  running: boolean;
  parts: RenderPart[];
}

/**
 * Tool calls are background noise: the reader only wants to know a turn is alive.
 *
 * `settled` matters because a call restored from the session log carries no outcome
 * (`ok: None` on the Rust side means "running *or* restored"), so history must never animate.
 */
export function toolActivity(calls: ToolCallWire[], settled: boolean): ToolActivity {
  const parts = calls.flatMap((call) => call.parts.filter((part) => part.kind !== "text"));
  const running = !settled && calls.some((call) => call.ok === null);
  return { running, parts };
}

/** A finished group with nothing to show leaves no row behind at all. */
export function showsToolActivity(activity: ToolActivity): boolean {
  return activity.running || activity.parts.length > 0;
}
