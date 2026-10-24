//! Content tool-calling protocol: declares a "text tool format + parsing method" for models
//! that **do not support native tool calling**.
//!
//! Background: Shirley's `ReAct` loop is driven entirely by the `tool_calls` field of
//! [`crate::message::Message::Assistant`]. As long as the model's **plain text** output can be
//! parsed into `tool_calls`, the loop needs no change. This module provides the pluggable
//! abstraction for that layer:
//!
//! - [`ToolCalling`]: the "tool transport mode" attached to [`crate::ModelConfig`] —
//!   `Native` (the status quo, structured `tools` / `tool_calls`) or `Content(protocol)`.
//! - [`ContentToolProtocol`]: the protocol interface — declares how to render the tool catalog,
//!   how to parse text, and how to feed results back.
//! - [`TaggedToolProtocol`]: the built-in **declarative** XML tag protocol (configurable tag
//!   names / feedback template).
//!
//! It is **orthogonal to** [`crate::ModelProtocol`]: content can combine with any underlying
//! protocol — it is a transport mode, not a protocol. All protocol differences are contained in
//! the adapter layer; the `Message` model and the runtime main loop are unchanged.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::message::{Message, ToolCall};
use crate::tool::{PROTOCOL_ERROR_TOOL, ToolDefinition};

/// The **body template** for the tool catalog + output contract (the `prompts/*.md` convention,
/// see `Agent.md`).
///
/// Body and code are separated: changing the wording only touches the md, and Rust just
/// `include_str!`s it and fills `{{placeholders}}`. For a non-English locale, ship another
/// template and swap this `include_str!` source (this is the minimal i18n seam for this layer).
const INSTRUCTIONS_TEMPLATE: &str = include_str!("../../prompts/tool_calling_instructions.md");

/// **Message templates** for correctable parse errors (`key=template` line format; lines
/// starting with `#` are comments).
const ERRORS_TEMPLATE: &str = include_str!("../../prompts/tool_calling_errors.md");

/// Injection character limit for the tool catalog (the "Available tools" section).
///
/// The fixed contract text is bounded and always emitted in full; only the catalog section,
/// which **grows with the number of tools**, gets truncated — with many tools (bash / read /
/// `web_search` / todo / skills …) it can expand significantly and crowd out the front of the
/// context. Over the limit it is truncated at a char boundary and **explicitly flagged** (not
/// silent), the same strategy as `todo`'s `MAX_RENDER_CHARS`.
const MAX_INSTRUCTIONS_CHARS: usize = 12_000;

/// Tool transport mode. Attached to `ModelConfig`, defaulting to [`ToolCallingMode::Native`].
///
/// This is the SDK's **only** public tool-protocol abstraction: callers just declare the mode;
/// the protocol instance, parser, and replay rendering are all constructed inside the adapter
/// from the mode, invisible to the outside.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallingMode {
    /// Native function calling: the request carries structured `tools`, and the response is
    /// parsed into structured `tool_calls`.
    #[default]
    Native,
    /// Built-in XML-tag text protocol: no `tools` are sent; the tool catalog is written into the
    /// request view's system prompt, and `tool_calls` are parsed out of the model's text.
    Tagged,
}

impl ToolCallingMode {
    /// Whether the text protocol is used.
    #[must_use]
    pub fn is_content(self) -> bool {
        !matches!(self, Self::Native)
    }

    /// Build the internal protocol instance for the mode (native mode returns `None`).
    pub(crate) fn protocol(self) -> Option<Arc<dyn ContentToolProtocol>> {
        match self {
            Self::Native => None,
            Self::Tagged => Some(Arc::new(TaggedToolProtocol::builder().build())),
        }
    }
}

/// A single parsed tool call. `arguments` is the **raw JSON string** (synonymous with
/// [`ToolCall::arguments`]); no type constraints are imposed here — a custom protocol cannot
/// guarantee the model passes correct arguments, so argument errors are fed back by the tool
/// layer as `ArgumentsError` (see decision 4 in `docs/tool-calling.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedToolCall {
    pub name: String,
    pub arguments: String,
}

/// The full product of one parse: thinking / display text / tool calls (0..n).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedDocument {
    pub thinking: Option<String>,
    pub description: Option<String>,
    pub calls: Vec<ParsedToolCall>,
    /// **Correctable problems** found during parsing (tool block missing name / unclosed params /
    /// invalid JSON). When non-empty, [`parse_content_response`] executes no call; instead it
    /// synthesizes a `__protocol__` feedback asking the model to resend (Step 5).
    pub errors: Vec<String>,
}

/// A pluggable content tool-calling protocol.
///
/// Implementations must be `Send + Sync` (the stream returned by `run_stream` is `Send`, and the
/// protocol is shared along with `ModelConfig`).
pub(crate) trait ContentToolProtocol: Send + Sync {
    /// Render the "tool catalog + output contract", which the application layer prepends to the
    /// system prompt.
    fn render_instructions(&self, tools: &[&ToolDefinition]) -> String;

    /// Parse tool calls / thinking / display text from the assistant's full text.
    fn parse(&self, text: &str) -> ParsedDocument;

    /// Re-render an assistant message as protocol text.
    ///
    /// Used when replaying history: `Message::Assistant` splits the structured fields
    /// (`reasoning_content` / `content` / `tool_calls`) apart, but the model needs to see the
    /// **original text of its own previous turn**; otherwise the tool-call chain breaks (the model
    /// sees "a result but no call").
    fn render_assistant(
        &self,
        thinking: Option<&str>,
        content: Option<&str>,
        calls: &[ToolCall],
    ) -> String;

    /// Render one tool result as a **feedback message** (models don't understand `role=tool`, so
    /// it is usually fed back as user text).
    fn render_result(&self, call: &ToolCall, output: &str) -> Message;

    /// Create a **streaming parser**: it consumes raw protocol text chunk by chunk and emits only
    /// visible increments (thinking / display text; tool blocks are suppressed). See
    /// [`ContentStreamParser`].
    ///
    /// By default it returns a "swallow everything" no-op parser — if a protocol author does not
    /// implement streaming parsing, no increment leaks out (the final state is still produced by
    /// [`Self::parse`] at `Finished`). That is, "not implementing" is a safe conservative default,
    /// rather than leaking the raw protocol text to the UI.
    fn stream_parser(&self) -> Box<dyn ContentStreamParser> {
        Box::new(NoopStreamParser)
    }
}

/// A **visible increment** emitted by streaming parsing — only the part meant for the user, never
/// protocol markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisibleChunk {
    /// Thinking / reasoning increment -> mapped to `AdapterEvent::ReasoningDelta`.
    Thinking(String),
    /// Display-text increment -> mapped to `AdapterEvent::ContentDelta`.
    Content(String),
}

/// **Streaming** protocol parser: translates the model's raw protocol text **incrementally** into
/// [`VisibleChunk`]s.
///
/// Complementary to [`ContentToolProtocol::parse`] (whole-segment parsing that produces
/// structured `tool_calls`):
/// - `parse` decides **control flow** (whether this turn has a tool call);
/// - `stream_parser` decides the **display stream** (streaming `thinking` / `description` to the
///   UI in real time, with `<tool>` blocks suppressed).
///
/// Implementations must accept **arbitrary fragments** (a single token split across several
/// chunks must still work) — the same origin as the char-level state machine underlying
/// [`TaggedToolProtocol`]. `Send` is required so the parser can travel with `invoke`'s stream
/// across threads.
pub(crate) trait ContentStreamParser: Send {
    /// Feed one raw text chunk and return the newly produced visible increments (possibly empty).
    fn push(&mut self, chunk: &str) -> Vec<VisibleChunk>;

    /// Input ends; flush any trailing residue (e.g. noise restored from an unclosed tag).
    fn finish(&mut self) -> Vec<VisibleChunk>;
}

/// Default streaming parser: swallow everything, leaking no increment.
struct NoopStreamParser;

impl ContentStreamParser for NoopStreamParser {
    fn push(&mut self, _chunk: &str) -> Vec<VisibleChunk> {
        Vec::new()
    }
    fn finish(&mut self) -> Vec<VisibleChunk> {
        Vec::new()
    }
}

fn default_result_template() -> String {
    r#"{"tool":"{tool}"}"#.to_string()
}

/// The built-in **declarative** XML tag protocol (derived from figmaagent's content protocol).
///
/// One assistant reply is an XML-like document:
///
/// ```text
/// <thinking>internal reasoning (optional)</thinking>
/// <description>reply to the user</description>
/// <tool>
///   <name>tool name</name>
///   <params>valid JSON</params>
/// </tool>
/// ```
///
/// Tag names and the feedback template can be overridden via the builder; for a fully custom
/// scheme (fenced-JSON / regex, etc.), just `impl ContentToolProtocol` yourself.
#[derive(bon::Builder, Clone)]
pub(crate) struct TaggedToolProtocol {
    /// Thinking tag name (default `thinking`).
    #[builder(into, default = "thinking".to_string())]
    thinking_tag: String,
    /// Display-text tag name (default `description`).
    #[builder(into, default = "description".to_string())]
    description_tag: String,
    /// Tool block tag name (default `tool`).
    #[builder(into, default = "tool".to_string())]
    tool_tag: String,
    /// Tool name tag name (default `name`).
    #[builder(into, default = "name".to_string())]
    name_tag: String,
    /// Tool arguments tag name (default `params`).
    #[builder(into, default = "params".to_string())]
    params_tag: String,
    /// Tool-result feedback template; `{tool}` is replaced with the tool name.
    #[builder(into, default = default_result_template())]
    result_template: String,
}

impl TaggedToolProtocol {
    fn tags(&self) -> TagConfig {
        TagConfig {
            thinking: self.thinking_tag.clone(),
            description: self.description_tag.clone(),
            tool: self.tool_tag.clone(),
            name: self.name_tag.clone(),
            params: self.params_tag.clone(),
        }
    }
}

impl ContentToolProtocol for TaggedToolProtocol {
    fn render_instructions(&self, tools: &[&ToolDefinition]) -> String {
        // The body lives in `prompts/tool_calling_instructions.md`; here we only fill placeholders.
        // The catalog section is rendered separately (with a length limit); the rest is the fixed
        // contract.
        let catalog = render_tool_catalog(tools);
        let result_header = self.result_template.replace("{tool}", "tool name");
        render_placeholders(
            INSTRUCTIONS_TEMPLATE,
            &[
                ("thinking", &self.thinking_tag),
                ("description", &self.description_tag),
                ("tool", &self.tool_tag),
                ("name", &self.name_tag),
                ("params", &self.params_tag),
                ("catalog", &catalog),
                ("result_header", &result_header),
            ],
        )
    }

    fn parse(&self, text: &str) -> ParsedDocument {
        parse_tagged(text, self.tags())
    }

    fn stream_parser(&self) -> Box<dyn ContentStreamParser> {
        Box::new(TaggedStreamParser::new(self.tags()))
    }

    fn render_assistant(
        &self,
        thinking: Option<&str>,
        content: Option<&str>,
        calls: &[ToolCall],
    ) -> String {
        let _ = thinking;
        let mut out = String::new();
        // **Do not replay `<thinking>`** (Step 6): native mode never sends back historical
        // reasoning anyway, and content mode should not carry it every turn either — otherwise the
        // context bloats and compression triggers earlier. Only the body and tool blocks are
        // replayed.
        if let Some(content) = content.filter(|text| !text.trim().is_empty()) {
            let _ = writeln!(
                out,
                "<{d}>{content}</{d}>",
                d = self.description_tag,
                content = escape_xml(content),
            );
        }
        // Note: `name` / `arguments` are **not escaped** — `arguments` is raw JSON, and escaping
        // would break round-trip parsing (the parse side cannot unescape params; they must stay
        // valid JSON). `<...>` inside strings is restored verbatim by the parser's
        // `params_json_in_string` + noise fallback.
        for call in calls {
            let _ = writeln!(
                out,
                "<{tool}>\n<{name}>{n}</{name}>\n<{p}>{args}</{p}>\n</{tool}>",
                tool = self.tool_tag,
                name = self.name_tag,
                p = self.params_tag,
                n = call.name,
                args = call.arguments,
            );
        }
        out.trim_end().to_string()
    }

    fn render_result(&self, call: &ToolCall, output: &str) -> Message {
        let header = self.result_template.replace("{tool}", &call.name);
        Message::User {
            content: format!("{header}\n{output}").into(),
        }
    }
}

/// Single-pass placeholder rendering: replace `{{key}}` with its value, leaving unknown
/// placeholders as-is.
///
/// Same semantics as `src/prompt_template.rs::render` (a minimal SDK-side implementation that
/// avoids an application-layer dependency). Only `{{...}}` is recognized; a single `{` (such as in
/// the JSON example `{"command":...}`) is emitted verbatim.
fn render_placeholders(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{'
            && index + 1 < bytes.len()
            && bytes[index + 1] == b'{'
            && let Some(offset) = template[index + 2..].find("}}")
        {
            let key = &template[index + 2..index + 2 + offset];
            if let Some((_, value)) = pairs.iter().find(|(name, _)| *name == key) {
                out.push_str(value);
                index += 2 + offset + 2;
                continue;
            }
        }
        let ch = template[index..]
            .chars()
            .next()
            .expect("index is on a char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Look up an error message template by `key` (a `key=template` line in `ERRORS_TEMPLATE`;
/// lines starting with `#` are comments).
fn error_template(key: &str) -> &'static str {
    ERRORS_TEMPLATE
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .find_map(|line| {
            let (name, template) = line.split_once('=')?;
            (name.trim() == key).then(|| template.trim())
        })
        .unwrap_or("")
}

/// Render a correctable error: fetch the template and fill placeholders (`{{tool}}` / `{{name}}` /
/// `{{params}}` / `{{tool_name}}`).
fn error_message(key: &str, pairs: &[(&str, &str)]) -> String {
    render_placeholders(error_template(key), pairs)
}

/// Render the "Available tools" catalog (parameters rendered as a compact doc rather than raw JSON
/// Schema) and apply the [`MAX_INSTRUCTIONS_CHARS`] limit.
fn render_tool_catalog(tools: &[&ToolDefinition]) -> String {
    let mut out = String::new();
    for (index, tool) in tools.iter().enumerate() {
        let _ = write!(out, "## {}. {}\n\n", index + 1, tool.name);
        if !tool.description.is_empty() {
            let _ = write!(out, "Description: {}\n\n", tool.description);
        }
        out.push_str("Parameters:\n```\n");
        out.push_str(&render_schema_doc(&tool.parameters));
        out.push_str("\n```\n\n");
    }
    if out.chars().count() <= MAX_INSTRUCTIONS_CHARS {
        return out;
    }
    // Over the limit: truncate at a char boundary and flag it explicitly (not silent), telling the
    // model the catalog is incomplete.
    let truncated: String = out.chars().take(MAX_INSTRUCTIONS_CHARS).collect();
    format!(
        "{truncated}\n…(tool catalog truncated: {} tools total, only the first part shown)\n",
        tools.len()
    )
}

/// Render a JSON Schema as a compact "JSON-Schema-ish doc" for the LLM.
///
/// Ported from figmaagent's `zod-to-doc.ts`: **readability and token friendliness take priority
/// over schema completeness**. It recursively walks the schema and renders each field as
/// `key?: type  // comment`: `enum` -> `a | b`; `number` with `>= / <=`; `string` with
/// `len / format`; objects / arrays recurse. Unusual structures fall back to `any`, guaranteed not
/// to panic.
fn render_schema_doc(schema: &Value) -> String {
    render_schema_node(schema, 0).join("\n")
}

/// Indent width (number of spaces). Set to 2, matching figmaagent.
const SCHEMA_INDENT: usize = 2;

fn schema_pad(level: usize) -> String {
    " ".repeat(level * SCHEMA_INDENT)
}

fn render_schema_node(node: &Value, level: usize) -> Vec<String> {
    if is_object_schema(node) {
        render_object_schema(node, level)
    } else {
        vec![format!("{}{}", schema_pad(level), type_label(node))]
    }
}

fn render_object_schema(node: &Value, level: usize) -> Vec<String> {
    let mut out = Vec::new();
    let open = if level == 0 {
        "{".to_string()
    } else {
        format!("{}{{", schema_pad(level))
    };
    out.push(open);

    let required: std::collections::HashSet<&str> = node
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(|item| item.as_str()).collect())
        .unwrap_or_default();

    if let Some(properties) = node.get("properties").and_then(|value| value.as_object()) {
        for (key, child) in properties {
            let opt_mark = if required.contains(key.as_str()) {
                ""
            } else {
                "?"
            };
            let comment = child
                .get("description")
                .and_then(|value| value.as_str())
                .map(|text| format!("  // {}", text.replace('\n', " ")))
                .unwrap_or_default();

            if is_object_schema(child) {
                out.push(format!(
                    "{}{}{}:{}",
                    schema_pad(level + 1),
                    key,
                    opt_mark,
                    comment
                ));
                out.extend(render_object_schema(child, level + 1));
            } else if is_array_of_objects(child) {
                out.push(format!(
                    "{}{}{}: [{}",
                    schema_pad(level + 1),
                    key,
                    opt_mark,
                    comment
                ));
                if let Some(item) = child.get("items") {
                    out.extend(render_object_schema(item, level + 1));
                }
                out.push(format!("{}]", schema_pad(level + 1)));
            } else {
                out.push(format!(
                    "{}{}{}: {}{}",
                    schema_pad(level + 1),
                    key,
                    opt_mark,
                    type_label(child),
                    comment
                ));
            }
        }
    }

    out.push(format!("{}}}", schema_pad(level)));
    out
}

fn is_object_schema(node: &Value) -> bool {
    node.get("type").and_then(|value| value.as_str()) == Some("object")
        || node.get("properties").is_some()
}

fn is_array_of_objects(node: &Value) -> bool {
    node.get("type").and_then(|value| value.as_str()) == Some("array")
        && node.get("items").is_some_and(is_object_schema)
}

fn type_label(node: &Value) -> String {
    if let Some(values) = node.get("enum").and_then(|value| value.as_array()) {
        return values
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(value) = node.get("const") {
        return value.to_string();
    }
    if let Some(items) = node.get("anyOf").and_then(|value| value.as_array()) {
        return items.iter().map(type_label).collect::<Vec<_>>().join(" | ");
    }
    if let Some(items) = node.get("oneOf").and_then(|value| value.as_array()) {
        return items.iter().map(type_label).collect::<Vec<_>>().join(" | ");
    }

    let type_str = match node.get("type") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => {
            return items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
        }
        _ => return "any".to_string(),
    };

    match type_str.as_str() {
        "string" => {
            let constraints = string_constraints(node);
            if constraints.is_empty() {
                "string".to_string()
            } else {
                format!("string ({constraints})")
            }
        }
        "number" | "integer" => {
            let constraints = number_constraints(node);
            if constraints.is_empty() {
                type_str
            } else {
                format!("{type_str} ({constraints})")
            }
        }
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item = node
                .get("items")
                .map_or_else(|| "any".to_string(), type_label);
            let min_items = node
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .map(|count| format!(", >={count} items"))
                .unwrap_or_default();
            format!("{item}[]{min_items}")
        }
        "object" => "object".to_string(),
        _ => "any".to_string(),
    }
}

// schemars injects sentinel values like +/-Number.MAX_SAFE_INTEGER into `int()`-style fields;
// these are noise with no guidance value for the LLM, so bounds near this boundary are ignored.
const NUMBER_SENTINEL: f64 = 9_007_199_254_740_991.0;

fn meaningful_bound(value: f64) -> bool {
    value.abs() < NUMBER_SENTINEL
}

fn number_constraints(node: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let bound = |key: &str| node.get(key).and_then(serde_json::Value::as_f64);
    if let Some(value) = bound("minimum").filter(|value| meaningful_bound(*value)) {
        parts.push(format!(">={value}"));
    }
    if let Some(value) = bound("exclusiveMinimum").filter(|value| meaningful_bound(*value)) {
        parts.push(format!(">{value}"));
    }
    if let Some(value) = bound("maximum").filter(|value| meaningful_bound(*value)) {
        parts.push(format!("<={value}"));
    }
    if let Some(value) = bound("exclusiveMaximum").filter(|value| meaningful_bound(*value)) {
        parts.push(format!("<{value}"));
    }
    parts.join(", ")
}

fn string_constraints(node: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(value) = node.get("minLength").and_then(serde_json::Value::as_u64) {
        parts.push(format!("len>={value}"));
    }
    if let Some(value) = node.get("maxLength").and_then(serde_json::Value::as_u64) {
        parts.push(format!("len<={value}"));
    }
    if let Some(value) = node.get("format").and_then(|value| value.as_str()) {
        parts.push(format!("format={value}"));
    }
    parts.join(", ")
}

/// **Before encoding**: map the working set into the content protocol's text view.
///
/// - **Every** `Assistant` (including plain replies with no tool calls) -> re-render via
///   [`ContentToolProtocol::render_assistant`] into the protocol's original text (the
///   `<thinking>` / `<description>` / `<tool>` tags are **preserved**). The model cannot see
///   structured `tool_calls`; it must see the **tagged original text** of its own previous turn —
///   first so the tool-call chain is not broken, second because the tags are themselves part of
///   the KV cache prefix: deterministic re-rendering keeps the cross-turn prefix byte-stable;
/// - `Message::Tool` -> rendered via [`ContentToolProtocol::render_result`] into a feedback
///   message (models don't understand `role=tool`; usually user text).
///
/// The tool name must be looked up by `id` from the **previous assistant**'s `tool_calls` —
/// `Message::Tool` only carries `tool_call_id`, not the name. When not found it degrades to an
/// empty name (which shows up as empty in the feedback template).
pub(crate) fn encode_messages_for_content(
    messages: &[Message],
    tools: &[&ToolDefinition],
    protocol: &dyn ContentToolProtocol,
) -> Vec<Message> {
    use std::collections::HashMap;

    let mut id_to_name: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::with_capacity(messages.len() + 1);
    // The tool catalog is appended after the first system message (the tool set is sorted by name
    // and stable within a session -> stable prefix). It only affects the request view and is not
    // written into `self.messages`.
    let instructions = if tools.is_empty() {
        String::new()
    } else {
        protocol.render_instructions(tools)
    };
    let mut messages = messages;
    if !instructions.trim().is_empty() {
        match messages.first() {
            Some(Message::System { content }) => {
                out.push(Message::System {
                    content: format!("{content}\n\n{instructions}"),
                });
                messages = &messages[1..];
            }
            _ => out.push(Message::System {
                content: instructions,
            }),
        }
    }
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        match message {
            // Every assistant is re-rendered into protocol text (preserving tags), not just the
            // one carrying tool calls.
            Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => {
                for call in tool_calls {
                    id_to_name.insert(call.id.as_str(), call.name.as_str());
                }
                let text = protocol.render_assistant(
                    reasoning_content.as_deref(),
                    content.as_deref(),
                    tool_calls,
                );
                // A fully empty assistant (no thinking / content / calls) is left as-is, avoiding
                // replaying a blank segment.
                if text.is_empty() {
                    out.push(message.clone());
                } else {
                    out.push(Message::Assistant {
                        content: Some(text),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                    });
                }
                index += 1;
            }
            // **Adjacent `Message::Tool` entries from the same turn are merged into a single user
            // message** (result blocks separated by `\n\n`). One assistant turn may call several
            // tools concurrently, and the runtime writes several consecutive Tool messages; if each
            // were fed back as its own user message, multiple consecutive user messages would
            // appear — protocols requiring alternating roles (e.g. Anthropic) would error.
            Message::Tool { .. } => {
                let mut blocks: Vec<String> = Vec::new();
                let mut passthrough: Vec<Message> = Vec::new();
                while index < messages.len() {
                    let Message::Tool {
                        tool_call_id,
                        content,
                    } = &messages[index]
                    else {
                        break;
                    };
                    // When the tool name is missing, use an explicit placeholder (not an empty
                    // string) to avoid feeding back `{"tool":""}` (Step 5).
                    let name = id_to_name
                        .get(tool_call_id.as_str())
                        .copied()
                        .unwrap_or("<unknown>");
                    let call = ToolCall {
                        id: tool_call_id.clone(),
                        name: name.to_string(),
                        arguments: String::new(),
                    };
                    let output = content
                        .as_ref()
                        .map(super::super::message::MessageContent::as_text)
                        .unwrap_or_default();
                    let rendered = protocol.render_result(&call, &output);
                    // The built-in protocol feeds back user text; non-text rendering (a custom
                    // protocol) degrades to passing each entry through individually.
                    match rendered {
                        Message::User { content } => blocks.push(content.as_text()),
                        Message::System { content } => blocks.push(content),
                        other => passthrough.push(other),
                    }
                    index += 1;
                }
                if !blocks.is_empty() {
                    out.push(Message::User {
                        content: blocks.join("\n\n").into(),
                    });
                }
                out.extend(passthrough);
            }
            other => {
                out.push(other.clone());
                index += 1;
            }
        }
    }
    out
}

/// **After decoding**: normalize the `Assistant` message returned by the model per the content
/// protocol.
///
/// If the decoder already produced structured `tool_calls` (which should not happen, but keep it
/// idempotent) it is returned as-is; otherwise `thinking` / `description` / `calls` are parsed out
/// of `content`:
/// - `description` -> `content` (the plain text shown to the user, with the XML shell stripped);
/// - `thinking` -> `reasoning_content` (**only when native reasoning is empty**, see below);
/// - `calls` -> `tool_calls` (`id` is generated with uuid; the protocol itself has no id).
///
/// A plain reply with no tags (neither `description` nor `calls`) **keeps its original text**, so a
/// normal answer is not eaten.
///
/// **Parse-error feedback** (Step 5): when `document.errors` is non-empty, no call is executed;
/// instead a call to the reserved tool [`PROTOCOL_ERROR_TOOL`] is synthesized — the runtime can
/// treat it as a tool result and feed it back to the model with no change (see Step 5 in
/// `docs/tool-calling-refactor.md`).
pub(crate) fn parse_content_response(
    protocol: &dyn ContentToolProtocol,
    message: Message,
) -> Message {
    let Message::Assistant {
        content,
        reasoning_content,
        tool_calls,
    } = message
    else {
        return message;
    };
    if !tool_calls.is_empty() {
        return Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
        };
    }
    let Some(text) = content.as_deref() else {
        return Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
        };
    };
    let document = protocol.parse(text);

    // Parse failure: synthesize one correctable feedback call; execute no real tool.
    if !document.errors.is_empty() {
        let detail = document.errors.join("；");
        return Message::Assistant {
            content: document.description,
            reasoning_content: merge_reasoning(reasoning_content, document.thinking),
            tool_calls: vec![protocol_error_call(&detail)],
        };
    }

    let calls: Vec<ToolCall> = document
        .calls
        .into_iter()
        .map(|call| ToolCall {
            id: format!("content_call_{}", Uuid::new_v4()),
            name: call.name,
            arguments: call.arguments,
        })
        .collect();
    let content = if calls.is_empty() {
        // Plain reply: prefer description; if absent, keep the original text.
        document.description.or(content)
    } else {
        // Tool turn: only description becomes the body, avoiding leaking the XML shell to the UI.
        document.description
    };
    Message::Assistant {
        content,
        // Thinking-source priority (Step 5): native reasoning wins; `<thinking>` only fills a gap.
        reasoning_content: merge_reasoning(reasoning_content, document.thinking),
        tool_calls: calls,
    }
}

/// Native reasoning wins; the protocol `<thinking>` is used only when it is empty.
fn merge_reasoning(native: Option<String>, tagged: Option<String>) -> Option<String> {
    match native.filter(|text| !text.trim().is_empty()) {
        Some(native) => Some(native),
        None => tagged,
    }
}

/// Build a synthetic call to the reserved tool [`PROTOCOL_ERROR_TOOL`] to feed the error details
/// back to the model.
fn protocol_error_call(detail: &str) -> ToolCall {
    let message = format!(
        "The tool call could not be parsed: {detail}. Please resend a **complete, valid** call: one `<tool>` block, \
containing `<name>` and valid `<params>` JSON, with all tags closed."
    );
    ToolCall {
        id: format!("content_call_{}", Uuid::new_v4()),
        name: PROTOCOL_ERROR_TOOL.to_string(),
        arguments: serde_json::json!({ "message": message }).to_string(),
    }
}

/// Tag-name configuration (owned, so it can be boxed as `'static` along with the streaming parser).
#[derive(Clone)]
struct TagConfig {
    thinking: String,
    description: String,
    tool: String,
    name: String,
    params: String,
}

impl TagConfig {
    fn known(&self, name: &str) -> bool {
        name == self.thinking.as_str()
            || name == self.description.as_str()
            || name == self.tool.as_str()
            || name == self.name.as_str()
            || name == self.params.as_str()
    }

    /// Whether `parent` may directly contain `child`: only `tool` can contain `name` / `params`.
    fn can_contain(&self, parent: &str, child: &str) -> bool {
        parent == self.tool.as_str()
            && (child == self.name.as_str() || child == self.params.as_str())
    }
}

/// Char-level parse state machine: supports arbitrary fragments (`feed` is fed char by char, state
/// persists across calls).
///
/// v1 parses only at the terminal state (`parse` runs once over the full text), but it is
/// implemented as a state machine to leave a seam for later streaming parsing.
struct TagParser {
    cfg: TagConfig,
    mode: Mode,
    /// The tag token currently being built (the content between `<` and `>`).
    token: String,
    /// The stack of open tags (each with its own content buffer).
    stack: Vec<OpenTag>,
    /// Stray text outside tags (used for noise fallback).
    outside: String,
    thinking: Option<String>,
    description: Option<String>,
    tool_name: Option<String>,
    tool_params: Option<String>,
    params_json_in_string: bool,
    params_json_escape: bool,
    in_code_fence: bool,
    backtick_run: usize,
    calls: Vec<ParsedToolCall>,
    /// **Correctable errors** found during parsing (missing name / unclosed params / invalid JSON).
    errors: Vec<String>,
    /// Streaming visible increments (accumulated char by char; the `parse_tagged` path ignores them).
    emitted: Vec<VisibleChunk>,
    /// Buffered trailing whitespace: emitted only when immediately followed by non-whitespace, and
    /// dropped at a tag boundary / at the end — so the streamed visible text matches the final
    /// parse's `trim()` result (newlines between tags are not treated as body text).
    pending_ws: String,
}

struct OpenTag {
    name: String,
    buf: String,
}

enum Mode {
    Text,
    /// A `<` was just read; whether it truly starts a tag is not yet decided (determines noise
    /// fallback vs. entering a tag).
    TagOpen,
    Tag,
}

/// Visible-increment channel: `Thinking` (-> `ReasoningDelta`) / `Content` (-> `ContentDelta`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum VisibleChannel {
    Thinking,
    Content,
}

fn parse_tagged(text: &str, cfg: TagConfig) -> ParsedDocument {
    if !starts_with_protocol_tag(text.trim_start(), &cfg) {
        if has_unfenced_tool_tag(text, &cfg.tool) {
            return ParsedDocument {
                errors: vec![error_message(
                    "leading_text",
                    &[("tool", &cfg.tool), ("description", &cfg.description)],
                )],
                ..ParsedDocument::default()
            };
        }
        return ParsedDocument {
            description: Some(text.to_string()),
            ..ParsedDocument::default()
        };
    }
    let mut parser = TagParser::new(cfg);
    for ch in text.chars() {
        parser.feed(ch);
    }
    parser.finish()
}

fn has_unfenced_tool_tag(text: &str, tool_tag: &str) -> bool {
    let opening = format!("<{tool_tag}>");
    let mut fence = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(marker) = marker {
            match fence {
                None => fence = Some(marker),
                Some(open) if open == marker => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_none() && line.len() - trimmed.len() < 4 && trimmed.starts_with(&opening) {
            return true;
        }
    }
    false
}

fn starts_with_protocol_tag(text: &str, cfg: &TagConfig) -> bool {
    [&cfg.thinking, &cfg.description, &cfg.tool]
        .into_iter()
        .any(|tag| text.starts_with(&format!("<{tag}>")))
}

impl TagParser {
    fn new(cfg: TagConfig) -> Self {
        Self {
            cfg,
            mode: Mode::Text,
            token: String::new(),
            stack: Vec::new(),
            outside: String::new(),
            thinking: None,
            description: None,
            tool_name: None,
            tool_params: None,
            params_json_in_string: false,
            params_json_escape: false,
            in_code_fence: false,
            backtick_run: 0,
            calls: Vec::new(),
            errors: Vec::new(),
            emitted: Vec::new(),
            pending_ws: String::new(),
        }
    }

    fn feed(&mut self, ch: char) {
        match self.mode {
            Mode::Text => {
                // Note: this does **not** short-circuit `'<'` on `params_json_in_string` —
                // otherwise an unmatched quote inside the string would keep `</params>` from ever
                // entering tag parsing (Step 5's quote fallback depends on it). A `<` inside a
                // string is restored as text by `resolve_tag`'s quote fallback.
                if ch == '<' && !self.in_code_fence {
                    self.mode = Mode::TagOpen;
                    self.token.clear();
                } else {
                    self.push_text(ch);
                }
            }
            // The first char after `<` decides the destination:
            //   - a valid tag-start char (or a closing `/`) -> enter the tag token;
            //   - `>` -> empty tag `<>`, skip;
            //   - another `<` -> the previous `<` was noise, restart the tag;
            //   - anything else (space, digit, punctuation …) -> `<` is plain text, roll back the
            //     whole thing.
            Mode::TagOpen => {
                if ch == '>' {
                    self.mode = Mode::Text;
                } else if ch == '<' {
                    self.push_noise("<");
                    self.token.clear();
                } else if is_name_start(ch) || ch == '/' {
                    self.token.push(ch);
                    self.mode = Mode::Tag;
                } else {
                    self.push_noise("<");
                    self.push_text(ch);
                    self.mode = Mode::Text;
                }
            }
            Mode::Tag => {
                if ch == '>' {
                    let raw = std::mem::take(&mut self.token);
                    self.mode = Mode::Text;
                    self.resolve_tag(&raw);
                } else {
                    self.token.push(ch);
                }
            }
        }
    }

    /// Append a plain char to "the content of the currently open tag" or the "outside-tags
    /// buffer".
    fn push_text(&mut self, ch: char) {
        if self
            .stack
            .last()
            .is_none_or(|tag| tag.name == self.cfg.description || tag.name == self.cfg.thinking)
        {
            if ch == '`' {
                self.backtick_run += 1;
                if self.backtick_run == 3 {
                    self.in_code_fence = !self.in_code_fence;
                }
            } else {
                self.backtick_run = 0;
            }
        }
        if self
            .stack
            .last()
            .is_some_and(|tag| tag.name == self.cfg.params)
        {
            if self.params_json_escape {
                self.params_json_escape = false;
            } else if ch == '\\' && self.params_json_in_string {
                self.params_json_escape = true;
            } else if ch == '"' {
                self.params_json_in_string = !self.params_json_in_string;
            }
        }
        match self.stack.last_mut() {
            Some(top) => top.buf.push(ch),
            None => self.outside.push(ch),
        }
        self.emit_char(ch);
    }

    /// Restore a piece of noise (an unrecognized tag) verbatim as text.
    fn push_noise(&mut self, text: &str) {
        match self.stack.last_mut() {
            Some(top) => top.buf.push_str(text),
            None => self.outside.push_str(text),
        }
        for ch in text.chars() {
            self.emit_char(ch);
        }
    }

    /// The **visible channel** corresponding to the current top-of-stack tag (`None` = suppressed).
    ///
    /// Text outside tags (empty stack) also returns `None`: aligned with the final parse's
    /// "tool turn keeps only `<description>`" (Step 5) — newlines / stray text between `<tool>`
    /// tags no longer leak out as `ContentDelta`.
    fn visible_channel(&self) -> Option<VisibleChannel> {
        match self.stack.last() {
            Some(top) if top.name == self.cfg.thinking => Some(VisibleChannel::Thinking),
            Some(top) if top.name == self.cfg.description => Some(VisibleChannel::Content),
            _ => None,
        }
    }

    /// Emit visible increments char by char (suppressed channels are dropped outright).
    ///
    /// Whitespace is buffered first ([`Self::pending_ws`]): emitted only when immediately followed
    /// by non-whitespace, dropped at a tag boundary / at the end — so newlines between tags are not
    /// treated as body text, and the streamed result matches the final `trim()`.
    fn emit_char(&mut self, ch: char) {
        let Some(channel) = self.visible_channel() else {
            self.pending_ws.clear();
            return;
        };
        if ch.is_whitespace() {
            self.pending_ws.push(ch);
            return;
        }
        if !self.pending_ws.is_empty() {
            let ws = std::mem::take(&mut self.pending_ws);
            self.push_visible(channel, &ws);
        }
        self.push_visible(channel, &ch.to_string());
    }

    /// Append a piece of text to the given visible channel (adjacent same-channel pieces merge).
    fn push_visible(&mut self, channel: VisibleChannel, text: &str) {
        match self.emitted.last_mut() {
            Some(VisibleChunk::Thinking(buf)) if channel == VisibleChannel::Thinking => {
                buf.push_str(text);
            }
            Some(VisibleChunk::Content(buf)) if channel == VisibleChannel::Content => {
                buf.push_str(text);
            }
            _ => self.emitted.push(match channel {
                VisibleChannel::Thinking => VisibleChunk::Thinking(text.to_string()),
                VisibleChannel::Content => VisibleChunk::Content(text.to_string()),
            }),
        }
    }

    /// Take the accumulated visible increments.
    fn take_visible(&mut self) -> Vec<VisibleChunk> {
        std::mem::take(&mut self.emitted)
    }

    fn resolve_tag(&mut self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // `<>` / `</>`: skip.
            return;
        }
        let self_closing = trimmed.ends_with('/');
        let body = trimmed.trim_end_matches('/').trim();
        let (closing, name) = match body.strip_prefix('/') {
            Some(rest) => (true, rest.trim()),
            None => (false, body),
        };
        if name.is_empty() {
            return;
        }
        // Unmatched-quote fallback (Step 5): when a string quote inside `<params>` is unclosed,
        // `params_json_in_string` stays stuck at true and `</params>` would be swallowed as string
        // content. When the top of stack is params and we are still "inside a string", on seeing
        // `</params>` we first run a JSON check: if it parses, close normally; otherwise restore the
        // whole thing as text.
        if self.params_json_in_string
            && closing
            && name == self.cfg.params
            && self
                .stack
                .last()
                .is_some_and(|tag| tag.name == self.cfg.params)
        {
            let buffer = self
                .stack
                .last()
                .map(|tag| tag.buf.clone())
                .unwrap_or_default();
            if serde_json::from_str::<serde_json::Value>(buffer.trim()).is_err() {
                self.push_noise(&format!("<{raw}>"));
                return;
            }
        }
        // Invalid / unknown tag: restore the whole thing as text (e.g. a bare `<` in `a < b`).
        if !is_valid_name(name) || !self.cfg.known(name) {
            self.push_noise(&format!("<{raw}>"));
            return;
        }
        if let Some(parent) = self.stack.last().map(|tag| tag.name.as_str()) {
            let closes_parent = closing && parent == name;
            let tool_child = parent == self.cfg.tool
                && !closing
                && (name == self.cfg.name || name == self.cfg.params);
            if !closes_parent && !tool_child {
                self.push_noise(&format!("<{raw}>"));
                return;
            }
        } else if closing
            || (name != self.cfg.thinking && name != self.cfg.description && name != self.cfg.tool)
        {
            self.push_noise(&format!("<{raw}>"));
            return;
        }
        // Only genuine structural tags drop boundary whitespace; XML examples in the body keep
        // their existing whitespace.
        self.pending_ws.clear();
        if closing {
            self.close_tag(name);
        } else {
            self.open_tag(name);
            if self_closing {
                self.close_tag(name);
            }
        }
    }

    fn open_tag(&mut self, name: &str) {
        // Fallback cleanup of invalid nesting; protocol tags in the body were already rolled back
        // to text in resolve_tag.
        while let Some(top) = self.stack.last() {
            if self.cfg.can_contain(&top.name, name) {
                break;
            }
            self.finalize_top();
        }
        if name == self.cfg.params {
            self.params_json_in_string = false;
            self.params_json_escape = false;
        }
        if name == self.cfg.description || name == self.cfg.thinking {
            self.in_code_fence = false;
            self.backtick_run = 0;
        }
        self.stack.push(OpenTag {
            name: name.to_string(),
            buf: String::new(),
        });
    }

    fn close_tag(&mut self, name: &str) {
        match self.stack.iter().rposition(|tag| tag.name == name) {
            Some(pos) => {
                while self.stack.len() > pos + 1 {
                    self.finalize_top();
                }
                self.finalize_top();
            }
            // No matching open tag found: restore as text.
            None => self.push_noise(&format!("</{name}>")),
        }
    }

    fn finalize_top(&mut self) {
        let Some(top) = self.stack.pop() else {
            return;
        };
        let content = top.buf.trim().to_string();
        if top.name == self.cfg.description || top.name == self.cfg.thinking {
            self.in_code_fence = false;
            self.backtick_run = 0;
        }
        if top.name == self.cfg.thinking {
            // Visible increments were already emitted char by char in `emit_char`; popping only
            // discards the buffer, re-emitting nothing.
            append_opt(&mut self.thinking, content);
        } else if top.name == self.cfg.description {
            // Unescape (symmetric with `render_assistant`'s escape_xml, Step 6).
            append_opt(&mut self.description, unescape_xml(&content));
        } else if top.name == self.cfg.name {
            self.tool_name = Some(content);
        } else if top.name == self.cfg.params {
            self.tool_params = Some(content);
        } else if top.name == self.cfg.tool {
            let name = self.tool_name.take().unwrap_or_default();
            let params = self.tool_params.take();
            // Parse errors are not silent (Step 5): missing name / invalid params JSON are recorded
            // in errors, and `parse_content_response` synthesizes a correctable feedback instead of
            // executing with garbage arguments.
            if name.is_empty() {
                self.errors
                    .push(error_message("missing_name", &[("name", &self.cfg.name)]));
            } else {
                let arguments = match params {
                    Some(raw) if !raw.trim().is_empty() => {
                        if serde_json::from_str::<serde_json::Value>(raw.trim()).is_err() {
                            self.errors.push(error_message(
                                "invalid_params",
                                &[("tool_name", &name), ("params", &self.cfg.params)],
                            ));
                        }
                        raw
                    }
                    _ => "{}".to_string(),
                };
                self.calls.push(ParsedToolCall { name, arguments });
            }
        }
    }

    fn finish(&mut self) -> ParsedDocument {
        // Drop the trailing buffered whitespace (aligned with `trim()`).
        self.pending_ws.clear();
        // Input ended while still building a tag: an unclosed `<...` / an isolated `<` is restored
        // as text.
        match self.mode {
            Mode::Tag => {
                let token = std::mem::take(&mut self.token);
                self.push_noise(&format!("<{token}"));
            }
            Mode::TagOpen => self.push_noise("<"),
            Mode::Text => {}
        }
        let complete_call_count = self.calls.len();
        let has_unclosed_tool = self.stack.iter().any(|tag| tag.name == self.cfg.tool);
        let truncated_in_params = self.stack.iter().any(|tag| tag.name == self.cfg.params);
        while !self.stack.is_empty() {
            self.finalize_top();
        }
        if has_unclosed_tool {
            self.calls.truncate(complete_call_count);
            self.errors.push(if truncated_in_params {
                error_message("truncated_params", &[("params", &self.cfg.params)])
            } else {
                error_message("unclosed_tool", &[("tool", &self.cfg.tool)])
            });
        }
        // Stray text outside tags is dropped uniformly (aligned with the streaming visible_channel,
        // Step 5).
        ParsedDocument {
            thinking: self.thinking.take(),
            description: self.description.take(),
            calls: std::mem::take(&mut self.calls),
            errors: std::mem::take(&mut self.errors),
        }
    }
}

/// The **streaming** parser for [`TaggedToolProtocol`]: wraps the same char-level state machine as
/// a [`ContentStreamParser`], fed raw text chunk by chunk and emitting only `thinking` /
/// `description` increments.
struct TaggedStreamParser {
    parser: TagParser,
    prefix: String,
    protocol_response: Option<bool>,
}

impl TaggedStreamParser {
    fn new(cfg: TagConfig) -> Self {
        Self {
            parser: TagParser::new(cfg),
            prefix: String::new(),
            protocol_response: None,
        }
    }
}

impl ContentStreamParser for TaggedStreamParser {
    fn push(&mut self, chunk: &str) -> Vec<VisibleChunk> {
        if self.protocol_response == Some(false) {
            return vec![VisibleChunk::Content(chunk.to_string())];
        }
        if self.protocol_response.is_none() {
            self.prefix.push_str(chunk);
            let trimmed = self.prefix.trim_start();
            let opening_tags = [
                format!("<{}>", self.parser.cfg.thinking),
                format!("<{}>", self.parser.cfg.description),
                format!("<{}>", self.parser.cfg.tool),
            ];
            if opening_tags
                .iter()
                .any(|tag| tag == trimmed || trimmed.starts_with(tag))
            {
                self.protocol_response = Some(true);
                let prefix = std::mem::take(&mut self.prefix);
                for ch in prefix.chars() {
                    self.parser.feed(ch);
                }
                return self.parser.take_visible();
            }
            if trimmed.is_empty() || opening_tags.iter().any(|tag| tag.starts_with(trimmed)) {
                return Vec::new();
            }
            self.protocol_response = Some(false);
            return vec![VisibleChunk::Content(std::mem::take(&mut self.prefix))];
        }
        for ch in chunk.chars() {
            self.parser.feed(ch);
        }
        self.parser.take_visible()
    }

    fn finish(&mut self) -> Vec<VisibleChunk> {
        if self.protocol_response != Some(true) {
            return if self.prefix.is_empty() {
                Vec::new()
            } else {
                vec![VisibleChunk::Content(std::mem::take(&mut self.prefix))]
            };
        }
        // Flush the state machine (unclosed tags / isolated `<` restored as noise, possibly
        // producing visible increments).
        self.parser.finish();
        self.parser.take_visible()
    }
}

/// Escape XML metacharacters so `<` / `&` in replayed text are not treated as protocol tags
/// (Step 6). Strictly inverse to [`unescape_xml`] on the parse side.
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Unescape the entities written by [`escape_xml`] (in reverse order).
fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn append_opt(slot: &mut Option<String>, content: String) {
    if content.is_empty() {
        return;
    }
    match slot {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(&content);
        }
        None => *slot = Some(content),
    }
}

/// Tag-name validity: the first char is a `NameStartChar`, the rest are `NameChar` (a pragmatic
/// subset of W3C XML).
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if is_name_start(first) => chars.all(is_name_char),
        _ => false,
    }
}

/// A pragmatic subset of W3C XML `NameStartChar`: letters / `_` / `:`.
fn is_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == ':'
}

/// A pragmatic subset of W3C XML `NameChar`: `NameStartChar` + digits / `-` / `.`.
fn is_name_char(ch: char) -> bool {
    is_name_start(ch) || ch.is_ascii_digit() || ch == '-' || ch == '.'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> TaggedToolProtocol {
        TaggedToolProtocol::builder().build()
    }

    #[test]
    fn parses_full_document() {
        let doc = protocol().parse(
            "<thinking>let me think</thinking>\n<description>let me take a look</description>\n<tool>\n<name>read</name>\n<params>{\"path\":\"a.rs\"}</params>\n</tool>",
        );
        assert_eq!(doc.thinking.as_deref(), Some("let me think"));
        assert_eq!(doc.description.as_deref(), Some("let me take a look"));
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "read".into(),
                arguments: "{\"path\":\"a.rs\"}".into(),
            }]
        );
    }

    #[test]
    fn protocol_tags_inside_json_string_remain_arguments() {
        for arguments in [
            r#"{"command":"printf '<tool><name>bash</name><params>{}</params></tool>'"}"#,
            r#"{"command":"printf \"<tool>\" && printf '</params>'"}"#,
        ] {
            let doc = protocol().parse(&format!(
                "<tool><name>bash</name><params>{arguments}</params></tool>"
            ));
            assert_eq!(doc.calls.len(), 1);
            assert_eq!(doc.calls[0].name, "bash");
            assert_eq!(doc.calls[0].arguments, arguments);
        }
    }

    #[test]
    fn calls_from_separate_responses_have_distinct_ids() {
        let response = || Message::Assistant {
            content: Some("<tool><name>bash</name><params>{}</params></tool>".into()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let Message::Assistant {
            tool_calls: first, ..
        } = parse_content_response(&protocol(), response())
        else {
            panic!("expected assistant")
        };
        let Message::Assistant {
            tool_calls: second, ..
        } = parse_content_response(&protocol(), response())
        else {
            panic!("expected assistant")
        };
        assert_ne!(first[0].id, second[0].id);
    }

    #[test]
    fn figma_responses_xml_becomes_a_tool_call() {
        let response = super::super::responses::decode_response(serde_json::json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text":
                    "<thinking>check the path</thinking><description>let me read the file first</description><tool><name>read</name><params>{\"path\":\"a.txt\"}</params></tool>"
                }]
            }]
        })).unwrap();
        let message = parse_content_response(&protocol(), response.message);
        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = message
        else {
            panic!("expected assistant")
        };
        assert_eq!(content.as_deref(), Some("let me read the file first"));
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn description_only_has_no_calls() {
        let doc = protocol().parse("<description>answer directly, no tool call</description>");
        assert_eq!(
            doc.description.as_deref(),
            Some("answer directly, no tool call")
        );
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn parses_multiple_independent_top_level_calls() {
        let doc = protocol().parse(concat!(
            "<description>Checking both files</description>",
            "<tool><name>read</name><params>{\"path\":\"a.rs\"}</params></tool>",
            "<tool><name>read</name><params>{\"path\":\"b.rs\"}</params></tool>",
        ));
        assert!(doc.errors.is_empty());
        assert_eq!(doc.calls.len(), 2);
        assert_eq!(doc.calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert_eq!(doc.calls[1].arguments, r#"{"path":"b.rs"}"#);
    }

    #[test]
    fn leading_prose_before_tool_requests_reformat_without_running_it() {
        let response = Message::Assistant {
            content: Some(
                concat!(
                    "I'll note the task.\n\n",
                    "<tool>\n<name>todo</name>\n",
                    "<params>{\"goal\":\"design sub-agent\"}</params>\n</tool>",
                )
                .into(),
            ),
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let Message::Assistant { tool_calls, .. } = parse_content_response(&protocol(), response)
        else {
            panic!("expected assistant");
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, crate::tool::PROTOCOL_ERROR_TOOL);
        assert!(tool_calls[0].arguments.contains("preceded by plain text"));
    }

    #[test]
    fn nested_protocol_tag_is_literal_until_parent_closes() {
        let doc = protocol().parse("<thinking>reasoning<description>answer</description>");
        assert_eq!(
            doc.thinking.as_deref(),
            Some("reasoning<description>answer</description>")
        );
        assert!(doc.description.is_none());
    }

    #[test]
    fn fenced_xml_example_is_not_a_tool_call() {
        let example = "Example:\n```xml\n<tool><name>bash</name><params>{}</params></tool>\n```";
        let doc = protocol().parse(example);
        assert_eq!(doc.description.as_deref(), Some(example));
        assert!(doc.calls.is_empty());

        let wrapped = format!("<description>{example}</description>");
        let doc = protocol().parse(&wrapped);
        assert_eq!(doc.description.as_deref(), Some(example));
        assert!(doc.calls.is_empty());
        let (_, streamed) = collect_visible(&protocol(), &[&wrapped]);
        assert_eq!(streamed, example);

        let outside = "<description>Example</description>\n```xml\n<tool><name>bash</name><params>{}</params></tool>\n```".to_string();
        assert!(protocol().parse(&outside).calls.is_empty());

        let inline = "<description>call example:\n<tool><name>bash</name><params>{}</params></tool></description>";
        assert!(protocol().parse(inline).calls.is_empty());
        let (_, streamed) = collect_visible(&protocol(), &[inline]);
        assert_eq!(
            streamed,
            "call example:\n<tool><name>bash</name><params>{}</params></tool>"
        );
    }

    #[test]
    fn unclosed_tool_block_does_not_execute() {
        let doc = protocol().parse("<tool><name>bash</name><params>{}</params>");
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn truncated_params_records_error() {
        // Truncated by max_tokens: `<params>` is unclosed -> not executed, and recorded in errors (Step 5).
        let doc = protocol().parse("<tool><name>bash</name><params>{\"command\":\"ls");
        assert!(doc.calls.is_empty());
        assert!(
            !doc.errors.is_empty(),
            "should record a truncation error: {doc:?}"
        );
    }

    #[test]
    fn missing_name_records_error() {
        let doc = protocol().parse("<tool><params>{}</params></tool>");
        assert!(doc.calls.is_empty());
        assert!(
            doc.errors.iter().any(|e| e.contains("name")),
            "should record a missing name: {doc:?}"
        );
    }

    #[test]
    fn invalid_params_json_records_error() {
        let doc = protocol().parse("<tool><name>bash</name><params>{not json}</params></tool>");
        assert!(
            !doc.errors.is_empty(),
            "should record invalid JSON: {doc:?}"
        );
    }

    #[test]
    fn unmatched_quote_in_params_falls_back_to_text() {
        // When a string quote is unclosed, `</params>` should not be swallowed; invalid JSON -> restore as text + record an error.
        let doc =
            protocol().parse("<tool><name>bash</name><params>{\"command\":\"ls}</params></tool>");
        assert!(
            doc.calls.is_empty(),
            "invalid JSON should not be executed: {doc:?}"
        );
        assert!(
            !doc.errors.is_empty(),
            "should record invalid JSON: {doc:?}"
        );
    }

    #[test]
    fn parse_error_produces_protocol_recovery_call() {
        // Parse error -> synthesize one `__protocol__` feedback call; execute no real tool (Step 5).
        let message = Message::Assistant {
            content: Some("<tool><params>{}</params></tool>".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let Message::Assistant { tool_calls, .. } = parse_content_response(&protocol(), message)
        else {
            panic!("expected assistant");
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, crate::tool::PROTOCOL_ERROR_TOOL);
        assert!(tool_calls[0].arguments.contains("message"));
    }

    #[test]
    fn native_reasoning_wins_over_tagged_thinking() {
        // Thinking-source priority: native reasoning wins; `<thinking>` only fills a gap (Step 5).
        let message = Message::Assistant {
            content: Some(
                "<thinking>tag thinking</thinking><description>answer</description>".into(),
            ),
            reasoning_content: Some("native thinking".into()),
            tool_calls: Vec::new(),
        };
        let Message::Assistant {
            reasoning_content, ..
        } = parse_content_response(&protocol(), message)
        else {
            panic!("expected assistant");
        };
        assert_eq!(reasoning_content.as_deref(), Some("native thinking"));

        // When native is empty, use `<thinking>`.
        let message = Message::Assistant {
            content: Some(
                "<thinking>tag thinking</thinking><description>answer</description>".into(),
            ),
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let Message::Assistant {
            reasoning_content, ..
        } = parse_content_response(&protocol(), message)
        else {
            panic!("expected assistant");
        };
        assert_eq!(reasoning_content.as_deref(), Some("tag thinking"));
    }

    #[test]
    fn stream_discards_text_outside_tags() {
        // Stray text outside tags is dropped uniformly (aligned with the final parse, Step 5).
        let protocol = protocol();
        let (thinking, content) = collect_visible(
            &protocol,
            &[
                "<description>answer</description>\n",
                "some stray text\n",
                "<tool><name>bash</name><params>{}</params></tool>",
            ],
        );
        assert_eq!(thinking, "");
        assert_eq!(content, "answer");
    }

    #[test]
    fn bare_angle_bracket_falls_back_to_text() {
        // `a < b` is not a valid tag; it should be restored as text into description.
        let doc = protocol().parse("<description>compare a < b</description>");
        assert_eq!(doc.description.as_deref(), Some("compare a < b"));
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn self_closing_tag_is_skipped() {
        let doc = protocol().parse("<description>hi</description><tool/>");
        assert_eq!(doc.description.as_deref(), Some("hi"));
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn params_defaults_to_empty_object() {
        let doc = protocol().parse("<tool><name>noop</name></tool>");
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "noop".into(),
                arguments: "{}".into(),
            }]
        );
    }

    #[test]
    fn unmatched_closing_tag_is_noise() {
        let doc = protocol().parse("<description>a</description></foo>");
        assert_eq!(doc.description.as_deref(), Some("a"));
    }

    #[test]
    fn split_feeding_is_stateful() {
        // Feeding char by char (simulating arbitrary fragments) matches the whole-parse result.
        let text = "<tool><name>x</name><params>{}</params></tool>";
        let proto = protocol();
        let whole = parse_tagged(text, proto.tags());
        let mut parser = TagParser::new(proto.tags());
        for ch in text.chars() {
            parser.feed(ch);
        }
        assert_eq!(parser.finish(), whole);
    }

    /// Concatenate the visible increments emitted by the streaming parser into (thinking, content) text.
    fn collect_visible(protocol: &TaggedToolProtocol, pieces: &[&str]) -> (String, String) {
        let mut parser = protocol.stream_parser();
        let mut thinking = String::new();
        let mut content = String::new();
        let absorb = |chunks: Vec<VisibleChunk>, thinking: &mut String, content: &mut String| {
            for chunk in chunks {
                match chunk {
                    VisibleChunk::Thinking(text) => thinking.push_str(&text),
                    VisibleChunk::Content(text) => content.push_str(&text),
                }
            }
        };
        for piece in pieces {
            absorb(parser.push(piece), &mut thinking, &mut content);
        }
        absorb(parser.finish(), &mut thinking, &mut content);
        (thinking, content)
    }

    #[test]
    fn stream_parser_emits_visible_text_and_hides_tool_block() {
        let protocol = protocol();
        let (thinking, content) = collect_visible(
            &protocol,
            &[
                "<thinking>think a bit</thinking>\n<description>doing it now</description>\n<tool><name>read</name><params>{\"path\":\"a.rs\"}</params></tool>",
            ],
        );
        // thinking / description increments are visible; the `<tool>` block is fully suppressed.
        assert_eq!(thinking, "think a bit");
        assert_eq!(content, "doing it now");
        assert!(!content.contains("read"));
        assert!(!content.contains("tool"));
    }

    #[test]
    fn stream_parser_is_split_insensitive() {
        // Feeding the same document in arbitrary fragments yields the same concatenated increments (including tool-block suppression).
        let protocol = protocol();
        let doc = "<thinking>think</thinking><description>answer</description><tool><name>bash</name><params>{}</params></tool>";
        let one_shot = collect_visible(&protocol, &[doc]);
        let per_char: Vec<String> = doc.chars().map(|c| c.to_string()).collect();
        let per_char: Vec<&str> = per_char.iter().map(String::as_str).collect();
        let split = collect_visible(&protocol, &per_char);
        assert_eq!(one_shot, split);
        assert_eq!(one_shot.1, "answer");
        assert_eq!(one_shot.0, "think");
    }

    #[test]
    fn stream_parser_passes_through_plain_text() {
        // Plain text (no tags) flows out directly as display text, not swallowed.
        let protocol = protocol();
        let (thinking, content) = collect_visible(&protocol, &["hello, ", "world"]);
        assert_eq!(thinking, "");
        assert_eq!(content, "hello, world");
    }

    #[test]
    fn default_stream_parser_swallows_everything() {
        // A protocol with no streaming parsing implemented: the default no-op parser leaks no raw text (a safe conservative default).
        struct Bare;
        impl ContentToolProtocol for Bare {
            fn render_instructions(&self, _tools: &[&ToolDefinition]) -> String {
                String::new()
            }
            fn parse(&self, _text: &str) -> ParsedDocument {
                ParsedDocument::default()
            }
            fn render_assistant(
                &self,
                _t: Option<&str>,
                _c: Option<&str>,
                _calls: &[ToolCall],
            ) -> String {
                String::new()
            }
            fn render_result(&self, _call: &ToolCall, _output: &str) -> Message {
                Message::User {
                    content: String::new().into(),
                }
            }
        }
        let mut parser = Bare.stream_parser();
        assert!(parser.push("<tool><name>x</name></tool>").is_empty());
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn custom_tags_are_honored() {
        let p = TaggedToolProtocol::builder()
            .tool_tag("call")
            .name_tag("fn")
            .params_tag("args")
            .build();
        let doc = p.parse("<call><fn>bash</fn><args>{\"command\":\"ls\"}</args></call>");
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }]
        );
    }

    #[test]
    fn render_result_uses_template() {
        let call = ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        };
        let msg = protocol().render_result(&call, "hello");
        match msg {
            Message::User { content } => {
                let content = content.as_text();
                assert!(content.contains("\"tool\":\"bash\""));
                assert!(content.ends_with("hello"));
            }
            other => panic!("expected a User feedback message, actual: {other:?}"),
        }
    }

    #[test]
    fn render_assistant_round_trips_through_parse() {
        let proto = protocol();
        let calls = vec![ToolCall {
            id: "content_call_0".into(),
            name: "bash".into(),
            arguments: "{\"command\":\"ls\"}".into(),
        }];
        // thinking is no longer replayed (Step 6): carrying historical reasoning every turn would bloat the context.
        let text = proto.render_assistant(Some("think a bit"), Some("look at the catalog"), &calls);
        assert!(
            !text.contains("think a bit"),
            "should not replay thinking: {text}"
        );
        let doc = proto.parse(&text);
        assert_eq!(doc.thinking.as_deref(), None);
        assert_eq!(doc.description.as_deref(), Some("look at the catalog"));
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }]
        );
    }

    /// Replay XML-escapes the **body** (arguments stay raw JSON), so text containing `<` round-trips losslessly (Step 6).
    #[test]
    fn render_assistant_escapes_content_and_round_trips() {
        let proto = protocol();
        let content = "compare a < b and x & y";
        let args = r#"{"command":"echo '<tool>'"}"#;
        let calls = vec![ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: args.into(),
        }];
        let text = proto.render_assistant(None, Some(content), &calls);
        assert!(
            text.contains("&lt;"),
            "should escape `<` in the body: {text}"
        );
        let doc = proto.parse(&text);
        assert_eq!(doc.description.as_deref(), Some(content));
        assert_eq!(doc.calls.len(), 1);
        assert_eq!(doc.calls[0].arguments, args);
    }

    /// Replay preserves tags: a plain assistant (no `tool_calls`) is also re-rendered into protocol
    /// text, rather than passing through the shell-stripped plain text — otherwise the
    /// `<description>` tag would be lost and the KV prefix would drift every turn.
    #[test]
    fn encode_keeps_tags_on_plain_assistant() {
        let proto = protocol();
        let messages = vec![
            Message::User {
                content: "hello".into(),
            },
            Message::Assistant {
                content: Some("I'm here".into()),
                reasoning_content: Some("think a bit".into()),
                tool_calls: Vec::new(),
            },
        ];
        let out = encode_messages_for_content(&messages, &[], &proto);
        // user is preserved as-is.
        assert!(matches!(&out[0], Message::User { content } if content == "hello"));
        // assistant is re-rendered into tagged original text (the tags must appear).
        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = &out[1]
        else {
            panic!("expected assistant");
        };
        let text = content.as_deref().unwrap();
        // thinking is not replayed (Step 6), but the description tag is kept.
        assert!(
            !text.contains("<thinking>"),
            "should not replay thinking: {text}"
        );
        assert!(
            text.contains("<description>I'm here</description>"),
            "description tag: {text}"
        );
        assert!(tool_calls.is_empty());
    }

    /// Replay preserves tags: an assistant with tool calls and its subsequent tool results are both re-rendered correctly.
    #[test]
    fn encode_rerenders_tool_call_and_result() {
        let proto = protocol();
        let messages = vec![
            Message::Assistant {
                content: Some("take a look".into()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "content_call_0".into(),
                    name: "bash".into(),
                    arguments: "{\"command\":\"ls\"}".into(),
                }],
            },
            Message::Tool {
                tool_call_id: "content_call_0".into(),
                content: Some("file.txt".into()),
            },
        ];
        let out = encode_messages_for_content(&messages, &[], &proto);
        let Message::Assistant { content, .. } = &out[0] else {
            panic!("expected assistant");
        };
        let text = content.as_deref().unwrap();
        assert!(text.contains("<tool>"), "tool tag: {text}");
        assert!(text.contains("<name>bash</name>"), "tool name: {text}");
        // tool results are rendered per the template (the default template is `{"tool":"{tool}"}` + newline + output).
        let Message::User { content } = &out[1] else {
            panic!("expected user (rendered tool result)");
        };
        let content = content.as_text();
        assert!(
            content.contains("{\"tool\":\"bash\"}"),
            "feedback header: {content}"
        );
        assert!(content.contains("file.txt"), "feedback body: {content}");
    }

    /// One assistant turn calling several tools concurrently -> the runtime writes several consecutive
    /// `Message::Tool` entries; when encoding they must be **merged into a single user message**
    /// (`\n\n`-separated), otherwise protocols requiring alternating roles (e.g. Anthropic) error
    /// on multiple consecutive user messages (docs/tool-calling-refactor.md Step 4).
    #[test]
    fn encode_merges_consecutive_tool_results_into_one_user() {
        let proto = protocol();
        let messages = vec![
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall {
                        id: "content_call_0".into(),
                        name: "bash".into(),
                        arguments: "{\"command\":\"ls\"}".into(),
                    },
                    ToolCall {
                        id: "content_call_1".into(),
                        name: "read".into(),
                        arguments: "{\"path\":\"a.rs\"}".into(),
                    },
                ],
            },
            Message::Tool {
                tool_call_id: "content_call_0".into(),
                content: Some("file.txt".into()),
            },
            Message::Tool {
                tool_call_id: "content_call_1".into(),
                content: Some("fn main() {}".into()),
            },
        ];
        let out = encode_messages_for_content(&messages, &[], &proto);
        // assistant + one merged user (not two users).
        assert_eq!(
            out.len(),
            2,
            "multiple tool results should be merged, actual output: {out:?}"
        );
        assert!(matches!(&out[0], Message::Assistant { .. }));
        let Message::User { content } = &out[1] else {
            panic!("expected single merged user, got {:?}", out[1]);
        };
        let content = content.as_text();
        // Both result blocks are present, each headed per the template.
        assert!(
            content.contains("{\"tool\":\"bash\"}"),
            "missing bash block: {content}"
        );
        assert!(
            content.contains("file.txt"),
            "missing bash output: {content}"
        );
        assert!(
            content.contains("{\"tool\":\"read\"}"),
            "missing read block: {content}"
        );
        assert!(
            content.contains("fn main() {}"),
            "missing read output: {content}"
        );
        // The two blocks are separated by a blank line.
        assert_eq!(
            content.matches("{\"tool\":").count(),
            2,
            "there should be two blocks: {content}"
        );
    }

    #[test]
    fn render_instructions_lists_tools() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "run a command".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let text = protocol().render_instructions(&[&def]);
        assert!(text.contains("bash"));
        assert!(text.contains("run a command"));
        assert!(text.contains("<tool>"));
        // The four-section structure modeled on figmaagent (contract locked in to prevent silent regressions).
        assert!(
            text.contains("# Tool-calling protocol"),
            "missing the how-it-works section: {text}"
        );
        assert!(
            text.contains("# Output format (follow strictly)"),
            "missing the output-format section: {text}"
        );
        assert!(
            text.contains("# Available tools"),
            "missing the tool-catalog section: {text}"
        );
        assert!(
            text.contains("# Tool-result feedback"),
            "missing the feedback-recognition section: {text}"
        );
        // Parameters are rendered as a compact doc (not raw JSON Schema).
        assert!(
            text.contains("Parameters:"),
            "missing the parameter doc: {text}"
        );
        // The contract must cover the "no tool needed, plain text directly" branch (otherwise the model may force out a tool block).
        assert!(
            text.contains("when you don't need a tool, just output plain text"),
            "missing the 'plain text when no tool needed' contract: {text}"
        );
        assert!(
            text.contains("When you don't need a tool**: just output a plain-text answer"),
            "missing the plain-text constraint entry: {text}"
        );
    }

    #[test]
    fn render_instructions_caps_the_tool_catalog() {
        // The catalog section exceeds the limit -> truncated at a char boundary and **explicitly flagged** (not silent).
        let huge = "x".repeat(MAX_INSTRUCTIONS_CHARS * 2);
        let def = ToolDefinition {
            name: "big".into(),
            description: huge,
            parameters: serde_json::json!({"type": "object"}),
        };
        let text = protocol().render_instructions(&[&def]);
        assert!(
            text.contains("tool catalog truncated"),
            "should explicitly flag truncation: {text}"
        );
        // The fixed contract is still present (only the catalog section, which grows with tool count, is truncated).
        assert!(text.contains("# Tool-calling protocol"));
        assert!(text.contains("# Output format (follow strictly)"));
        assert!(text.contains("# Tool-result feedback"));
    }

    /// JSON Schema -> compact doc: type / optional / comment / constraint / enum / nesting.
    #[test]
    fn schema_doc_renders_compact_readable() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "the command to run" },
                "timeout": { "type": "integer", "minimum": 1, "description": "timeout in seconds" },
                "mode": { "enum": ["fast", "safe"] }
            }
        });
        let doc = render_schema_doc(&schema);
        assert!(
            doc.contains("command: string"),
            "required field must have no ?: {doc}"
        );
        assert!(doc.contains("// the command to run"), "comment: {doc}");
        assert!(
            doc.contains("timeout?: integer (>=1)"),
            "optional+constraint: {doc}"
        );
        assert!(doc.contains("\"fast\" | \"safe\""), "enum: {doc}");
    }
}
