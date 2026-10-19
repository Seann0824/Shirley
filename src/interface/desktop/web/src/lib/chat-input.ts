import type { KeyboardEvent } from "react";

export function shouldSendChatOnEnter(event: KeyboardEvent<HTMLElement>) {
  if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return false;

  const usesTouchInput =
    typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;
  return !usesTouchInput;
}
