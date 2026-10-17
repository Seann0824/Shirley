import { useMemo } from "react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { structuredReportToMarkdown } from "@/lib/structuredReport";

/**
 * Markdown renderer for assistant **answer** text.
 *
 * Some answers are machine-readable XML reports (a `<summary>` / `<response>` wrapper with
 * `<status>` / `<goal>` / `<changes>` … sections). Because raw HTML is intentionally
 * disabled in `AiMarkdown`, those tags would otherwise be shown to the user verbatim. Here
 * we detect that shape and project it to readable markdown first; ordinary prose passes
 * through untouched (byte-for-byte). See `lib/structuredReport.ts`.
 */
export function AiAssistantMarkdown({
  content,
  streaming,
}: {
  content: string;
  streaming: boolean;
}) {
  const rendered = useMemo(() => structuredReportToMarkdown(content), [content]);
  return <AiMarkdown content={rendered} streaming={streaming} />;
}
