// Structured-report rendering.
//
// Some agent answers are not prose: the model returns a machine-readable XML report
// (a `<summary>`/`<response>` wrapper around `<status>` / `<goal>` / `<changes>` /
// `<verification>` / `<notes>` … sections). The markdown pipeline intentionally
// disables raw HTML (see `AiMarkdown`), so without this the tags would be escaped and
// shown to the user verbatim — the exact bug this module fixes.
//
// The converter is deliberately *tolerant*: the exact tag schema is owned by the outer
// harness and drifts between runs, so we never hard-code one schema. We parse a small,
// forgiving XML subset into a tree and render it back to clean markdown (headings +
// bullets), keeping every piece of text. Unknown or unbalanced `<…>` (e.g. `Vec<String>`,
// `<config_dir>` inside prose) is left as literal text, never treated as a tag.

export interface ReportSection {
  label: string;
  markdown: string;
}

export interface StructuredReport {
  /** Leading summary text, if the report has a dedicated `<summary>` child. */
  summary: string | null;
  sections: ReportSection[];
  /** Full markdown projection of the report (summary + sections). */
  markdown: string;
}

type TextNode = { kind: "text"; value: string };
type ElementNode = {
  kind: "element";
  tag: string;
  attrs: Record<string, string>;
  children: Node[];
};
type Node = TextNode | ElementNode;

const isElement = (n: Node): n is ElementNode => n.kind === "element";

// Tags we are willing to *recognise* even when we cannot find their closing tag yet.
// Everything else must be balanced (`<x>…</x>`) to count as a tag, so stray angle
// brackets in prose (generics, paths) stay literal.
const KNOWN_TAGS = new Set<string>([
  "response", "summary", "report", "result", "answer",
  "status", "goal", "current_state", "proposed_plan", "parallelization",
  "changes", "verification", "notes", "note", "subagent", "file", "build",
  "tests", "test", "check", "phase", "item", "step", "task", "tasks",
  "progress", "decisions", "decision", "open_questions", "next_steps",
  "plan", "findings", "constraints", "risks", "follow_ups",
]);

// Wrapper tags that mean "the whole message is one structured report".
const ROOT_TAGS = new Set<string>(["response", "summary", "report", "result", "answer"]);

const LABELS: Record<string, string> = {
  summary: "Summary", status: "Status", goal: "Goal", current_state: "Current state",
  proposed_plan: "Proposed plan", parallelization: "Parallelization", changes: "Changes",
  verification: "Verification", notes: "Notes", note: "Note", subagent: "Subagent",
  file: "File", build: "Build", tests: "Tests", test: "Test", check: "Check",
  phase: "Phase", item: "Item", step: "Step", task: "Task", tasks: "Tasks",
  progress: "Progress", decisions: "Decisions", decision: "Decision",
  open_questions: "Open questions", next_steps: "Next steps", plan: "Plan",
  findings: "Findings", constraints: "Constraints", risks: "Risks", follow_ups: "Follow-ups",
  response: "Response", report: "Report", result: "Result", answer: "Answer",
};

function humanizeTag(tag: string): string {
  const known = LABELS[tag];
  if (known) return known;
  return tag
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

function attrsLabel(attrs: Record<string, string>): string {
  const entries = Object.entries(attrs).filter(([key]) => key !== "id");
  if (entries.length === 0) return "";
  return ` (${entries.map(([key, value]) => `${key}: ${value}`).join(", ")})`;
}

const OPEN_TAG = /^<([a-zA-Z][a-zA-Z0-9_-]*)((?:\s[^<>]*?)?)(\/?)>/;
const CLOSE_TAG = /^<\/([a-zA-Z][a-zA-Z0-9_-]*)\s*>/;

function hasClosingTag(src: string, from: number, name: string): boolean {
  return new RegExp(`</${name}\\s*>`, "i").test(src.slice(from));
}

function parseAttrs(raw: string): Record<string, string> {
  const attrs: Record<string, string> = {};
  if (!raw.trim()) return attrs;
  const re = /([a-zA-Z_][\w-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(raw)) !== null) {
    attrs[match[1]] = match[2] ?? match[3] ?? match[4] ?? "";
  }
  return attrs;
}

/** Parse a forgiving XML subset into a node list. Never throws. */
export function parseNodes(src: string): Node[] {
  const root: ElementNode = { kind: "element", tag: "#root", attrs: {}, children: [] };
  const stack: ElementNode[] = [root];
  let text = "";
  let i = 0;

  const flush = () => {
    if (!text) return;
    stack[stack.length - 1].children.push({ kind: "text", value: text });
    text = "";
  };

  while (i < src.length) {
    const lt = src.indexOf("<", i);
    if (lt < 0) {
      text += src.slice(i);
      break;
    }
    text += src.slice(i, lt);

    if (src[lt + 1] === "/") {
      const close = CLOSE_TAG.exec(src.slice(lt));
      let idx = -1;
      if (close) {
        const name = close[1].toLowerCase();
        for (let depth = stack.length - 1; depth >= 1; depth -= 1) {
          if (stack[depth].tag === name) {
            idx = depth;
            break;
          }
        }
      }
      if (close && idx >= 1) {
        flush();
        stack.length = idx; // drop the matched element and everything above it
        i = lt + close[0].length;
      } else {
        text += "<";
        i = lt + 1;
      }
      continue;
    }

    const open = OPEN_TAG.exec(src.slice(lt));
    if (!open) {
      text += "<";
      i = lt + 1;
      continue;
    }
    const name = open[1].toLowerCase();
    const accepted =
      KNOWN_TAGS.has(name) || hasClosingTag(src, lt + open[0].length, name);
    if (!accepted) {
      text += "<";
      i = lt + 1;
      continue;
    }

    flush();
    const element: ElementNode = {
      kind: "element",
      tag: name,
      attrs: parseAttrs(open[2] ?? ""),
      children: [],
    };
    stack[stack.length - 1].children.push(element);
    if (open[3] !== "/") stack.push(element);
    i = lt + open[0].length;
  }

  flush();
  return root.children;
}

function ownText(node: ElementNode): string {
  return node.children
    .filter((child): child is TextNode => child.kind === "text")
    .map((child) => child.value)
    .join("")
    .trim();
}

/** A leaf has no element children; its body is plain text (possibly markdown). */
function isLeaf(node: ElementNode): boolean {
  return !node.children.some(isElement);
}

function renderLeafBody(text: string): string {
  return text.includes("\n") ? `\n\n${text}` : ` ${text}`;
}

function renderBulletLines(node: ElementNode, indent: number): string[] {
  const pad = "  ".repeat(indent);
  const label = humanizeTag(node.tag);
  const attr = attrsLabel(node.attrs);
  const body = ownText(node);

  if (isLeaf(node)) {
    if (!body) return [];
    return [`${pad}- **${label}**${attr}:${renderLeafBody(body).replace(/\n\n/g, " ")}`];
  }

  const lines = [`${pad}- **${label}**${attr}`];
  if (body) {
    for (const line of body.split("\n")) lines.push(`${pad}  ${line}`);
  }
  for (const child of node.children.filter(isElement)) {
    lines.push(...renderBulletLines(child, indent + 1));
  }
  return lines;
}

function renderSection(node: ElementNode): string {
  const label = humanizeTag(node.tag);
  const attr = attrsLabel(node.attrs);
  const body = ownText(node);

  if (isLeaf(node)) {
    if (!body) return "";
    return `**${label}**${attr}:${renderLeafBody(body)}`;
  }

  const lines = [`### ${label}${attr}`];
  if (body) {
    lines.push("", body);
  }
  const bullets = node.children.filter(isElement).flatMap((child) => renderBulletLines(child, 0));
  if (bullets.length > 0) {
    lines.push("", ...bullets);
  }
  return lines.join("\n");
}

/**
 * Detect whether `text` is essentially one structured XML report. Conservative: the
 * message must be a single recognised wrapper element (with element children) and no
 * meaningful prose outside it.
 */
export function isStructuredReport(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed.startsWith("<")) return false;

  const nodes = parseNodes(trimmed);
  const elements = nodes.filter(isElement);
  const hasProse = nodes.some((n) => n.kind === "text" && n.value.trim().length > 0);
  if (hasProse) return false;

  if (elements.length === 1) {
    const root = elements[0];
    if (!ROOT_TAGS.has(root.tag)) return false;
    return root.children.some(isElement);
  }

  // No single wrapper: accept a bare sequence of recognised sections (>=2).
  if (elements.length >= 2 && elements.every((el) => KNOWN_TAGS.has(el.tag))) {
    return elements.some((el) => el.children.some(isElement) || ownText(el).length > 0);
  }
  return false;
}

/** Parse a structured report into summary + sections + a ready-to-render markdown body. */
export function parseStructuredReport(text: string): StructuredReport | null {
  const trimmed = text.trim();
  if (!isStructuredReport(trimmed)) return null;

  const nodes = parseNodes(trimmed);
  const elements = nodes.filter(isElement);

  let container: ElementNode;
  if (elements.length === 1) {
    container = elements[0];
  } else {
    container = { kind: "element", tag: "#root", attrs: {}, children: elements };
  }

  const children = container.children.filter(isElement);

  // A dedicated `<summary>` child is pulled out as the leading paragraph. When the wrapper
  // itself *is* `<summary>`, its children are the sections (nothing to pull out).
  let summary: string | null = null;
  let sectionEls = children;
  if (container.tag !== "summary") {
    const summaryEl = children.find((el) => el.tag === "summary");
    if (summaryEl) {
      const text = ownText(summaryEl) || renderSection(summaryEl);
      summary = text.trim() || null;
      sectionEls = children.filter((el) => el !== summaryEl);
    }
  }

  const sections: ReportSection[] = sectionEls
    .map((el) => ({ label: humanizeTag(el.tag), markdown: renderSection(el) }))
    .filter((section) => section.markdown.length > 0);

  const parts: string[] = [];
  if (summary) parts.push(summary);
  for (const section of sections) parts.push(section.markdown);
  const markdown = parts.join("\n\n").trim();

  if (!markdown) return null;
  return { summary, sections, markdown };
}

/** Project a structured report to readable markdown. Returns the input unchanged if it is not one. */
export function structuredReportToMarkdown(text: string): string {
  return parseStructuredReport(text)?.markdown ?? text;
}
