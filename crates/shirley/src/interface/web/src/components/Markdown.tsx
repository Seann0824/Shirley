import type { ReactNode } from "react";

/**
 * Split a reply into the pieces shown as separate bubbles: paragraphs (blank-line separated),
 * with a fenced code block kept whole.
 */
export function splitBlocks(text: string): string[] {
  const blocks: string[] = [];
  let current: string[] = [];
  let fenced = false;
  const flush = () => {
    const block = current.join("\n").trim();
    if (block) blocks.push(block);
    current = [];
  };
  for (const line of text.split("\n")) {
    if (line.trimStart().startsWith("```")) {
      if (!fenced) flush();
      current.push(line);
      fenced = !fenced;
      if (!fenced) flush();
      continue;
    }
    if (!fenced && line.trim() === "") {
      flush();
      continue;
    }
    current.push(line);
  }
  flush();
  return blocks;
}

const INLINE = /(\*\*[^*\n]+\*\*|`[^`\n]+`|\[[^\]\n]+\]\((?:https?:\/\/)[^)\s]+\))/g;

function inline(text: string): ReactNode[] {
  return text.split(INLINE).map((piece, index) => {
    if (piece.startsWith("**") && piece.endsWith("**") && piece.length > 4) {
      return <strong key={index}>{piece.slice(2, -2)}</strong>;
    }
    if (piece.startsWith("`") && piece.endsWith("`") && piece.length > 2) {
      return <code key={index}>{piece.slice(1, -1)}</code>;
    }
    const link = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(piece);
    if (link) {
      return (
        <a key={index} href={link[2]} target="_blank" rel="noreferrer">
          {link[1]}
        </a>
      );
    }
    return piece;
  });
}

/** One block of chat markdown: a code fence, or lines with headings / bullets / inline marks. */
export function MarkdownBlock({ text }: { text: string }) {
  if (text.trimStart().startsWith("```")) {
    const body = text.replace(/^\s*```[^\n]*\n?/, "").replace(/\n?```\s*$/, "");
    return (
      <pre className="md-code">
        <code>{body}</code>
      </pre>
    );
  }
  return (
    <p className="md">
      {text.split("\n").map((line, index) => {
        const heading = /^#{1,6}\s+(.*)$/.exec(line);
        const bullet = /^\s*[-*]\s+(.*)$/.exec(line);
        return (
          <span className="md-line" key={index}>
            {heading ? <strong>{inline(heading[1])}</strong> : bullet ? <>• {inline(bullet[1])}</> : inline(line)}
          </span>
        );
      })}
    </p>
  );
}
