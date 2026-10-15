import { useMemo } from "react";
import {
  Streamdown,
  defaultRehypePlugins,
  defaultRemarkPlugins,
  type Components,
  type StreamdownProps,
  type StreamdownTranslations,
} from "streamdown";
import "streamdown/styles.css";
import { cn } from "@/ui/utils";
import { FlightMapRenderer } from "@/components/ai/FlightMapRenderer";

const BASE_REMARK_PLUGINS = Object.values(defaultRemarkPlugins);
const SAFE_REHYPE_PLUGINS = Object.entries(defaultRehypePlugins)
  .filter(([name]) => name !== "raw")
  .map(([, plugin]) => plugin);

// Streamdown's built-in link safety shows a confirmation modal. The product
// opens external links in a new tab with `rel="noreferrer"` instead, so the
// modal is disabled and links render through the shared component below.
const DISABLED_LINK_SAFETY = { enabled: false } as const;

const AI_MARKDOWN_TRANSLATIONS = {
  copied: "Copied",
  copyCode: "Copy code",
  copyTable: "Copy table",
  copyTableAsCsv: "Copy as CSV",
  copyTableAsMarkdown: "Copy as Markdown",
  copyTableAsTsv: "Copy as TSV",
  downloadFile: "Download file",
  downloadTable: "Download table",
  downloadTableAsCsv: "Download as CSV",
  downloadTableAsMarkdown: "Download as Markdown",
} satisfies Partial<StreamdownTranslations>;

export function aiMarkdownExternalLinkProps(className: string | undefined) {
  return {
    className: cn(
      "font-medium text-ink underline decoration-line underline-offset-4 outline-none transition-colors hover:decoration-ink focus-visible:ring-2 focus-visible:ring-ink/35",
      className,
    ),
    rel: "noreferrer",
    target: "_blank",
  };
}

// Custom fenced-code-block renderer: a ```flightmap block is handed to FlightMap to draw
// a static map. This is streamdown's public extension point (plugins.renderers) and does
// not touch the Message / event protocol — the map is just a code block in the body
// markdown, flowing through the existing text pipeline.
const MARKDOWN_PLUGINS: StreamdownProps["plugins"] = {
  renderers: [{ language: "flightmap", component: FlightMapRenderer }],
};

const BASE_COMPONENTS: Components = {
  a: ({ href, children, className, title }) => (
    <a href={href} title={title} {...aiMarkdownExternalLinkProps(className)}>
      {children}
    </a>
  ),
  h1: ({ children, className, node: _node, ...props }) => (
    <h3 {...props} className={cn("m-0 font-display text-title-sm font-normal", className)}>
      {children}
    </h3>
  ),
  h2: ({ children, className, node: _node, ...props }) => (
    <h3 {...props} className={cn("m-0 font-display text-base font-normal", className)}>
      {children}
    </h3>
  ),
  h3: ({ children, className, node: _node, ...props }) => (
    <h3 {...props} className={cn("m-0 font-display text-base font-normal", className)}>
      {children}
    </h3>
  ),
};

export function AiMarkdown({
  content,
  streaming,
  className,
  components,
  remarkPlugins,
}: {
  content: string;
  streaming: boolean;
  className?: string;
  components?: Components;
  remarkPlugins?: StreamdownProps["remarkPlugins"];
}) {
  const mergedPlugins = useMemo(
    () => [...BASE_REMARK_PLUGINS, ...(remarkPlugins ?? [])],
    [remarkPlugins],
  );
  const mergedComponents = useMemo(
    () => (components ? { ...BASE_COMPONENTS, ...components } : BASE_COMPONENTS),
    [components],
  );

  // Keep direction on the host: Streamdown's per-block auto-direction wrappers
  // use display: contents, which breaks the sibling spacing during streaming.
  return (
    <div className="min-w-0 max-w-full [overflow-wrap:anywhere]" dir="auto" data-ai-markdown>
      <Streamdown
        className={cn("min-w-0 max-w-full text-body-sm leading-relaxed text-ink", className)}
        components={mergedComponents}
        disallowedElements={["img"]}
        isAnimating={streaming}
        linkSafety={DISABLED_LINK_SAFETY}
        mode="streaming"
        parseIncompleteMarkdown={streaming}
        plugins={MARKDOWN_PLUGINS}
        rehypePlugins={SAFE_REHYPE_PLUGINS}
        remarkPlugins={mergedPlugins}
        tableMaxHeight="none"
        translations={AI_MARKDOWN_TRANSLATIONS}
        unwrapDisallowed
      >
        {content}
      </Streamdown>
    </div>
  );
}
