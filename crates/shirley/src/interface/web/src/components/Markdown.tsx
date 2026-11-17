import DOMPurify from "dompurify";
import { Marked } from "marked";

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

// GitHub-flavoured (tables, task lists, strikethrough), and a single newline is a line break,
// as people write in chat.
const markdown = new Marked({ gfm: true, breaks: true, async: false });

/** Markdown to safe HTML: model output is untrusted, so no scripts, no event handlers. */
export function renderMarkdown(text: string): string {
  const html = markdown.parse(text) as string;
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ["style", "form", "input", "button", "iframe"],
    FORBID_ATTR: ["style"],
  });
}

// Links open beside the app, never in place of it.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noreferrer noopener");
  }
});

/** One block of chat markdown, rendered. */
export function MarkdownBlock({ text }: { text: string }) {
  return <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(text) }} />;
}
