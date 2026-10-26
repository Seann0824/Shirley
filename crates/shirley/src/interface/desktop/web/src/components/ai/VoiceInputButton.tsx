import { Loader2, Mic, Square } from "lucide-react";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";
import type { VoiceInput } from "@/lib/useVoiceInput";

/**
 * Microphone button for the composer.
 *
 * One button and nothing else. Recording and transcription both happen on the user's own
 * device, so there is nothing to configure: the browser's recogniser when the webview has a
 * working one, this computer's whisper.cpp otherwise. Offering a picker turned a one-click
 * action into a configuration question with three technical answers.
 */
export function VoiceInputButton({ voice, className }: { voice: VoiceInput; className?: string }) {
  // Nothing on this runtime can transcribe: hide instead of showing a dead button.
  if (!voice.available) return null;

  const listening = voice.state === "listening";
  const transcribing = voice.state === "transcribing";
  const label = listening ? "Stop dictation" : transcribing ? "Transcribing" : "Dictate";

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-xs"
      aria-label={label}
      aria-pressed={listening}
      title={label}
      disabled={transcribing}
      className={cn(
        // The box stays 32px in every state: dictation must never resize the bar under the
        // user's finger while they are talking into it.
        "rounded-full text-muted hover:text-ink",
        // Recording is an active state, not a fault. Painting it with `danger` (this system's
        // error colour) read as "something broke"; a solid ink disc says "on air" instead,
        // and the muted pulse keeps it alive without moving any layout.
        listening && "bg-ink text-canvas hover:bg-ink hover:text-canvas motion-safe:animate-pulse",
        className,
      )}
      onClick={() => voice.toggle()}
    >
      {transcribing ? (
        <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
      ) : listening ? (
        // A square is the universal "stop" mark; the mic glyph would leave the second click
        // meaning "start again" in the user's head.
        <Square className="size-3 animate-pulse" aria-hidden="true" />
      ) : (
        <Mic className="size-4" aria-hidden="true" />
      )}
    </Button>
  );
}
