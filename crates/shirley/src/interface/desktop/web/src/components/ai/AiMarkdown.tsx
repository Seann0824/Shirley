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
import { code } from "@streamdown/code";
import { math } from "@streamdown/math";
import { mermaid } from "@streamdown/mermaid";
import { cjk } from "@streamdown/cjk";
import "katex/dist/katex.min.css";
import { cn } from "@/ui/utils";
import { FlightMapRenderer } from "@/components/ai/FlightMapRenderer";
import { ImageBlock } from "@/components/ai/ImageBlock";

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
  downloadDiagram: "Download diagram",
  downloadDiagramAsMmd: "Download as Mermaid",
  downloadDiagramAsPng: "Download as PNG",
  downloadDiagramAsSvg: "Download as SVG",
  mermaidFormatMmd: "Mermaid source",
  mermaidFormatPng: "PNG",
  mermaidFormatSvg: "SVG",
  viewFullscreen: "View fullscreen",
  exitFullscreen: "Exit fullscreen",
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

// Code highlighting (Shiki), math (KaTeX), diagrams (Mermaid) and CJK-aware
// emphasis parsing are shipped as separate Streamdown plugin packages. They are
// wired here through Streamdown's public `plugins` extension point; none of them
// touch the Message / event protocol — a fenced block is still just a code block
// in the body markdown flowing through the existing text pipeline.
//
// `flightmap` stays a custom fenced-block renderer (a ```flightmap block is
// handed to FlightMap to draw a static map) and shares the same renderer slot.
const MARKDOWN_PLUGINS: StreamdownProps["plugins"] = {
  code,
  math,
  mermaid,
  cjk,
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
  // A picture in the model's answer is a **block**, not a run of text: rounded, height-capped,
  // and clickable for the full-size view (the same `ImageBlock` the render parts use). It used
  // to be dropped outright by `disallowedElements`, so an `![shot](...)` simply vanished.
  img: ({ src, alt }) => (src ? <ImageBlock src={src} alt={alt ?? ""} /> : null),
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
