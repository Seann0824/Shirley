//! Content 工具调用协议：为**不支持原生 tool calling** 的模型声明「文本工具格式 + 解析方法」。
//!
//! 背景：Shirley 的 ReAct 循环完全由 [`crate::message::Message::Assistant`] 的
//! `tool_calls` 驱动。只要能把模型输出的**纯文本**解析成 `tool_calls`，循环就无需改动。
//! 本模块提供这一层的可插拔抽象：
//!
//! - [`ToolCalling`]：挂在 [`crate::ModelConfig`] 上的「工具传输模式」——
//!   `Native`（现状，结构化 `tools` / `tool_calls`）或 `Content(协议)`。
//! - [`ContentToolProtocol`]：协议接口——声明如何渲染工具目录、如何解析文本、如何回灌结果。
//! - [`TaggedToolProtocol`]：内置的**声明式** XML 标签协议（可换标签名 / 回灌模板）。
//!
//! 它**正交于** [`crate::ModelProtocol`]：content 可与任意底层协议组合，是传输模式而非协议。
//! 协议差异全部收敛在适配层，`Message` 模型与 runtime 主循环不改。

use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::message::{Message, ToolCall};
use crate::tool::{PROTOCOL_ERROR_TOOL, ToolDefinition};

/// 工具传输模式。挂在 `ModelConfig` 上，默认 [`ToolCallingMode::Native`]。
///
/// 这是 SDK 对外**唯一**的工具协议抽象：调用方只声明模式，协议实例、解析器、
/// 回放渲染全部在 adapter 内部按模式构造，外部不感知。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallingMode {
    /// 原生 function calling：请求带结构化 `tools`，响应解析结构化 `tool_calls`。
    #[default]
    Native,
    /// 内置 XML 标签文本协议：不发 `tools`，工具目录写进请求视图的 system；
    /// 从模型文本里解析出 `tool_calls`。
    Tagged,
}

impl ToolCallingMode {
    /// 是否走文本协议。
    pub fn is_content(self) -> bool {
        !matches!(self, Self::Native)
    }

    /// 按模式构造内部协议实例（原生模式返回 `None`）。
    pub(crate) fn protocol(self) -> Option<Arc<dyn ContentToolProtocol>> {
        match self {
            Self::Native => None,
            Self::Tagged => Some(Arc::new(TaggedToolProtocol::builder().build())),
        }
    }
}

/// 解析出的一次工具调用。`arguments` 是**原始 JSON 字符串**（与
/// [`ToolCall::arguments`] 同义），不在这里做类型约束——自研协议无法保证模型传参正确，
/// 参数错误交给工具层以 `ArgumentsError` 反馈（见 `docs/tool-calling.md` 决策 4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedToolCall {
    pub name: String,
    pub arguments: String,
}

/// 一次解析的完整产物：思考 / 展示文本 / 工具调用（0..n）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedDocument {
    pub thinking: Option<String>,
    pub description: Option<String>,
    pub calls: Vec<ParsedToolCall>,
    /// 解析中发现的**可纠正问题**（工具块缺 name / 未闭合 params / 非法 JSON）。
    /// 非空时 [`parse_content_response`] 不执行任何调用，而是合成一条
    /// `__protocol__` 回灌，请模型重发（Step 5）。
    pub errors: Vec<String>,
}

/// 可插拔的内容工具解析协议。
///
/// 实现必须是 `Send + Sync`（`run_stream` 返回的流是 `Send`，且协议随 `ModelConfig` 共享）。
pub(crate) trait ContentToolProtocol: Send + Sync {
    /// 渲染「工具目录 + 输出契约」，由应用层拼进 system prompt 前部。
    fn render_instructions(&self, tools: &[&ToolDefinition]) -> String;

    /// 从 assistant 的完整文本里解析工具调用 / 思考 / 展示文本。
    fn parse(&self, text: &str) -> ParsedDocument;

    /// 把一条 assistant 消息重新渲染成协议文本。
    ///
    /// 回放历史时用：`Message::Assistant` 把结构化字段（`reasoning_content` /
    /// `content` / `tool_calls`）拆开了，而模型需要看到它**自己上一轮输出的原文**，
    /// 否则工具调用链会断（模型看到"有结果、无调用"）。
    fn render_assistant(
        &self,
        thinking: Option<&str>,
        content: Option<&str>,
        calls: &[ToolCall],
    ) -> String;

    /// 把一次工具结果渲染成**回灌消息**（模型不认 `role=tool`，故通常回成 user 文本）。
    fn render_result(&self, call: &ToolCall, output: &str) -> Message;

    /// 创建一个**流式解析器**：逐块吃原始协议文本、只吐可见增量（思考 / 展示文本，
    /// 工具块屏蔽）。见 [`ContentStreamParser`]。
    ///
    /// 默认返回「吞掉一切」的空解析器——协议作者未实现流式解析时，增量不外泄
    /// （最终态仍由 [`Self::parse`] 在 `Finished` 给出）。即「不实现」是安全的保守
    /// 默认，而非把协议原文泄漏给 UI。
    fn stream_parser(&self) -> Box<dyn ContentStreamParser> {
        Box::new(NoopStreamParser)
    }
}

/// 流式解析吐出的**可见增量**——只含该给用户看的部分，绝不含协议标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisibleChunk {
    /// 思考 / 推理增量 → 映射为 `AdapterEvent::ReasoningDelta`。
    Thinking(String),
    /// 展示文本增量 → 映射为 `AdapterEvent::ContentDelta`。
    Content(String),
}

/// **流式**协议解析器：把模型吐出的原始协议文本**增量**翻译成 [`VisibleChunk`]。
///
/// 与 [`ContentToolProtocol::parse`]（整段解析、产出结构化 `tool_calls`）互补：
/// - `parse` 决定**控制流**（这轮有没有工具调用）；
/// - `stream_parser` 决定**显示流**（把 `thinking` / `description` 实时吐给 UI，
///   工具块 `<tool>` 屏蔽）。
///
/// 实现必须能接受**任意分片**（一个 token 被切成几块也要对）——与
/// [`TaggedToolProtocol`] 底层的字符级状态机同源。`Send` 是为了随 `invoke` 的流跨线程。
pub(crate) trait ContentStreamParser: Send {
    /// 喂入一块原始文本，返回本次新产生的可见增量（可能为空）。
    fn push(&mut self, chunk: &str) -> Vec<VisibleChunk>;

    /// 输入结束，冲刷尾部残留（如未闭合标签被还原的噪音）。
    fn finish(&mut self) -> Vec<VisibleChunk>;
}

/// 默认流式解析器：吞掉一切，不外泄任何增量。
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

/// 内置的**声明式** XML 标签协议（源自 figmaagent 的 content 协议）。
///
/// 一次 assistant 回复是一个 XML-like 文档：
///
/// ```text
/// <thinking>内部推理（可选）</thinking>
/// <description>给用户的回答</description>
/// <tool>
///   <name>工具名</name>
///   <params>合法 JSON</params>
/// </tool>
/// ```
///
/// 标签名与回灌模板都可经 builder 覆盖；要完全自定义（fenced-JSON / 正则等），
/// 自己 `impl ContentToolProtocol` 即可。
#[derive(bon::Builder, Clone)]
pub(crate) struct TaggedToolProtocol {
    /// 思考标签名（默认 `thinking`）。
    #[builder(into, default = "thinking".to_string())]
    thinking_tag: String,
    /// 展示文本标签名（默认 `description`）。
    #[builder(into, default = "description".to_string())]
    description_tag: String,
    /// 工具块标签名（默认 `tool`）。
    #[builder(into, default = "tool".to_string())]
    tool_tag: String,
    /// 工具名标签名（默认 `name`）。
    #[builder(into, default = "name".to_string())]
    name_tag: String,
    /// 工具参数标签名（默认 `params`）。
    #[builder(into, default = "params".to_string())]
    params_tag: String,
    /// 工具结果回灌模板，`{tool}` 会被替换成工具名。
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
        let thinking_tag = &self.thinking_tag;
        let description_tag = &self.description_tag;
        let tool_tag = &self.tool_tag;
        let name_tag = &self.name_tag;
        let params_tag = &self.params_tag;

        let mut out = String::new();

        // 一、运行机制（ReAct 循环）——让模型知道「输出 → 执行 → 回灌」是一个闭环。
        out.push_str("# 工具调用协议\n\n");
        out.push_str("你可以调用工具来完成任务。工具调用是一个多轮循环：\n");
        out.push_str("1. 用户发来一条消息；\n");
        out.push_str(&format!(
            "2. 你输出一段回复——**需要动手时**按下面的 XML 格式输出（含恰好一个 `<{tool_tag}>`）；**不需要工具时直接输出纯文本**即可；\n"
        ));
        out.push_str("3. 系统执行工具，并把结果作为新的 user 消息回灌给你；\n");
        out.push_str(&format!(
            "4. 你据结果判断任务是否完成——未完成就继续发 `<{tool_tag}>`，完成则直接给出最终回答（纯文本或 `<{description_tag}>` 皆可）。\n\n"
        ));

        // 二、输出格式契约。
        out.push_str("# 输出格式（务必严格遵守）\n\n");
        out.push_str(&format!(
            "<{thinking_tag}>内部推理（可选）</{thinking_tag}>\n<{description_tag}>给用户的回答</{description_tag}>\n<{tool_tag}>\n  <{name_tag}>工具名</{name_tag}>\n  <{params_tag}>合法的 JSON 对象</{params_tag}>\n</{tool_tag}>\n\n"
        ));
        out.push_str("约束：\n");
        out.push_str(&format!(
            "- **需要调用工具时**：每条回复**恰好一个** `<{tool_tag}>`，且标签之外不要输出任何字符（含空行、说明文字）。\n"
        ));
        out.push_str("- 工具调用必须写在回复顶层，完整闭合所有标签；不要把工具调用放进 Markdown 代码块或展示文本中。展示 XML 示例时请使用代码块，它只会作为文字显示。\n");
        out.push_str(&format!(
            "- `<{params_tag}>` 内必须是合法 JSON（双引号、转义字符，无注释）。\n"
        ));
        out.push_str("- 命令复杂时先拆成短调用；不要输出半截 JSON。字符串内的双引号和反斜杠必须按 JSON 转义。\n");
        out.push_str(&format!(
            "- 示例：`<{tool_tag}><{name_tag}>bash</{name_tag}><{params_tag}>{{\"command\":\"git status --short\"}}</{params_tag}></{tool_tag}>`。发送前检查 JSON 和所有标签都已闭合。\n"
        ));
        out.push_str(&format!(
            "- **不需要工具时**：直接输出纯文本回答即可，无需任何标签（也可用 `<{thinking_tag}>` / `<{description_tag}>` 包一层）。\n"
        ));
        out.push_str(&format!(
            "- **不发 `<{tool_tag}>` 就表示本轮结束**；想继续做事就必须发工具块。\n\n"
        ));

        // 三、工具目录（参数渲染成紧凑可读文档，而非原始 JSON Schema）。
        out.push_str("# 可用工具\n\n");
        out.push_str("每个工具的「参数」描述了 JSON 字段的类型与含义；请严格按描述传参，未列出的字段不要传。\n\n");
        for (index, tool) in tools.iter().enumerate() {
            out.push_str(&format!("## {}. {}\n\n", index + 1, tool.name));
            if !tool.description.is_empty() {
                out.push_str(&format!("描述：{}\n\n", tool.description));
            }
            out.push_str("参数：\n```\n");
            out.push_str(&render_schema_doc(&tool.parameters));
            out.push_str("\n```\n\n");
        }

        // 四、回灌识别规则——告诉模型「以结果头开头的 user 消息 = 工具返回值」。
        let sample_header = self.result_template.replace("{tool}", "工具名");
        out.push_str("# 工具结果回灌\n\n");
        out.push_str(&format!(
            "当你看到的最新 user 消息以 `{sample_header}` 开头时，这是上一次工具调用的返回值，**不是**用户的新指令；请据此判断下一步：成功则核对任务是否全部完成，失败则调整参数重试或说明原因。\n"
        ));
        // 只保留**通用**语义；「不要把模型服务自身的工作区当成用户项目环境」这类
        // 针对特定模型的判断属于业务，移到应用层 `src/prompt.rs`（Step 7）。
        out.push_str("只有收到对应的工具结果，才能说工具已执行。参数无效时命令没有执行，应重新发送完整、合法的调用。\n");

        out
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
        // **不回放 `<thinking>`**（Step 6）：原生模式历史 reasoning 本就不回传，content
        // 模式也不该每轮带上——否则上下文膨胀、更早触发压缩。仅回放正文与工具块。
        if let Some(content) = content.filter(|text| !text.trim().is_empty()) {
            out.push_str(&format!(
                "<{d}>{content}</{d}>\n",
                d = self.description_tag,
                content = escape_xml(content),
            ));
        }
        // 注意：`name` / `arguments` **不转义**——`arguments` 是 JSON 原文，转义会破坏往返
        // 解析（parse 侧无法反转义 params，它必须保持合法 JSON）。字符串内的 `<...>` 由
        // 解析器的 `params_json_in_string` + 噪音回退原样还原。
        for call in calls {
            out.push_str(&format!(
                "<{tool}>\n<{name}>{n}</{name}>\n<{p}>{args}</{p}>\n</{tool}>\n",
                tool = self.tool_tag,
                name = self.name_tag,
                p = self.params_tag,
                n = call.name,
                args = call.arguments,
            ));
        }
        out.trim_end().to_string()
    }

    fn render_result(&self, call: &ToolCall, output: &str) -> Message {
        let header = self.result_template.replace("{tool}", &call.name);
        Message::User {
            content: format!("{header}\n{output}"),
        }
    }
}

/// 把 JSON Schema 渲染成给 LLM 看的紧凑「JSON-Schema-ish 文档」。
///
/// 移植自 figmaagent 的 `zod-to-doc.ts`：**可读、token 友好优先于 schema 完整**。
/// 做法是递归遍历 schema，把每个字段渲染成 `key?: type  // 注释`：
/// `enum` → `a | b`；`number` 带 `>= / <=`；`string` 带 `len / format`；对象 / 数组递归。
/// 非常规结构回退到 `any`，保证不 panic。
fn render_schema_doc(schema: &Value) -> String {
    render_schema_node(schema, 0).join("\n")
}

/// 缩进宽度（空格数）。与 figmaagent 一致取 2。
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
            let opt_mark = if required.contains(key.as_str()) { "" } else { "?" };
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
        && node
            .get("items")
            .map(is_object_schema)
            .unwrap_or(false)
}

fn type_label(node: &Value) -> String {
    if let Some(values) = node.get("enum").and_then(|value| value.as_array()) {
        return values
            .iter()
            .map(|value| value.to_string())
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
                .map(type_label)
                .unwrap_or_else(|| "any".to_string());
            let min_items = node
                .get("minItems")
                .and_then(|value| value.as_u64())
                .map(|count| format!(", >={count} 个"))
                .unwrap_or_default();
            format!("{item}[]{min_items}")
        }
        "object" => "object".to_string(),
        _ => "any".to_string(),
    }
}

// schemars 会给 `int()` 类字段注入 ±Number.MAX_SAFE_INTEGER 之类的哨兵值，
// 对 LLM 无指导意义、纯属噪音，故忽略掉接近该边界的约束。
const NUMBER_SENTINEL: f64 = 9_007_199_254_740_991.0;

fn meaningful_bound(value: f64) -> bool {
    value.abs() < NUMBER_SENTINEL
}

fn number_constraints(node: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let bound = |key: &str| node.get(key).and_then(|value| value.as_f64());
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
    if let Some(value) = node.get("minLength").and_then(|value| value.as_u64()) {
        parts.push(format!("len>={value}"));
    }
    if let Some(value) = node.get("maxLength").and_then(|value| value.as_u64()) {
        parts.push(format!("len<={value}"));
    }
    if let Some(value) = node.get("format").and_then(|value| value.as_str()) {
        parts.push(format!("format={value}"));
    }
    parts.join(", ")
}

/// **编码前**：把工作集映射成 content 协议文本视图。
///
/// - **所有** `Assistant`（含无工具调用的普通回答）→ [`ContentToolProtocol::render_assistant`]
///   重新渲染成协议原文（`<thinking>` / `<description>` / `<tool>` 标签**保留**）。模型
///   看不到结构化 `tool_calls`，必须看到它自己上一轮输出的**带标签原文**——一是工具调用链
///   不断，二是标签本身是 KV 缓存前缀的一部分：确定性重渲染保证跨轮前缀逐字节稳定；
/// - `Message::Tool` → [`ContentToolProtocol::render_result`] 渲染成回灌消息
///   （模型不认 `role=tool`，通常是 user 文本）。
///
/// 工具名需要从**前一条 assistant** 的 `tool_calls` 里按 `id` 反查——`Message::Tool`
/// 只带 `tool_call_id`，不带名字。找不到时退化为空名（回灌模板里体现为空）。
pub(crate) fn encode_messages_for_content(
    messages: &[Message],
    tools: &[&ToolDefinition],
    protocol: &dyn ContentToolProtocol,
) -> Vec<Message> {
    use std::collections::HashMap;

    let mut id_to_name: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::with_capacity(messages.len() + 1);
    // 工具目录拼到首条 system 之后（工具集按名排序且会话内稳定 → 前缀稳定）。
    // 只作用于请求视图，不写进 `self.messages`。
    let instructions = if tools.is_empty() { String::new() } else { protocol.render_instructions(tools) };
    let mut messages = messages;
    if !instructions.trim().is_empty() {
        match messages.first() {
            Some(Message::System { content }) => {
                out.push(Message::System { content: format!("{content}\n\n{instructions}") });
                messages = &messages[1..];
            }
            _ => out.push(Message::System { content: instructions }),
        }
    }
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        match message {
            // 所有 assistant 都重渲染成协议原文（保留标签），不再只处理带工具调用的那条。
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
                // 全空的 assistant（无 thinking / content / 调用）保持原样，避免回放出一段空白。
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
            // **同一轮相邻的 `Message::Tool` 合并成一条 user 消息**（各结果块 `\n\n` 分隔）。
            // 一轮 assistant 可能并发调用多个工具，runtime 会落成多条连续 Tool 消息；若逐条
            // 回成 user，会连续出现多条 user 消息——Anthropic 等要求角色交替的协议会报错。
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
                    // 工具名缺失时用显式占位符（而非空串），避免回灌成 `{"tool":""}`（Step 5）。
                    let name = id_to_name
                        .get(tool_call_id.as_str())
                        .copied()
                        .unwrap_or("<unknown>");
                    let call = ToolCall {
                        id: tool_call_id.clone(),
                        name: name.to_string(),
                        arguments: String::new(),
                    };
                    let rendered = protocol.render_result(&call, content.as_deref().unwrap_or(""));
                    // 内置协议回灌的是 user 文本；非文本渲染（自定义协议）退化为逐条透传。
                    match rendered {
                        Message::User { content } | Message::System { content } => blocks.push(content),
                        other => passthrough.push(other),
                    }
                    index += 1;
                }
                if !blocks.is_empty() {
                    out.push(Message::User {
                        content: blocks.join("\n\n"),
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

/// **解码后**：把模型返回的 `Assistant` 消息按 content 协议规整化。
///
/// 若解码器已经产出结构化 `tool_calls`（不应发生，但保持幂等）则原样返回；
/// 否则从 `content` 解析出 `thinking` / `description` / `calls`：
/// - `description` → `content`（展示给用户的纯文本，剥掉 XML 外壳）；
/// - `thinking` → `reasoning_content`（**仅当原生 reasoning 为空时**，见下）；
/// - `calls` → `tool_calls`（`id` 用 uuid 生成，协议本身无 id）。
///
/// 无标签的普通回复（无 `description` 也无 `calls`）**保留原文**，避免把正常回答吃掉。
///
/// **解析错误回灌**（Step 5）：`document.errors` 非空时，不执行任何调用，改为合成
/// 一条对保留工具 [`PROTOCOL_ERROR_TOOL`] 的调用——runtime 无需改动即可把它当作一次
/// 工具结果回灌给模型（见 `docs/tool-calling-refactor.md` Step 5）。
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

    // 解析失败：合成一条可纠正的回灌调用，不执行任何真实工具。
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
        // 普通回复：优先 description，缺失则保留原文。
        document.description.or(content)
    } else {
        // 工具轮：只把 description 当正文，避免把 XML 外壳泄露给 UI。
        document.description
    };
    Message::Assistant {
        content,
        // 思考来源优先级（Step 5）：原生 reasoning 优先，`<thinking>` 仅补空。
        reasoning_content: merge_reasoning(reasoning_content, document.thinking),
        tool_calls: calls,
    }
}

/// 原生 reasoning 优先；仅在它为空时采用协议 `<thinking>`。
fn merge_reasoning(native: Option<String>, tagged: Option<String>) -> Option<String> {
    match native.filter(|text| !text.trim().is_empty()) {
        Some(native) => Some(native),
        None => tagged,
    }
}

/// 构造一条指向保留工具 [`PROTOCOL_ERROR_TOOL`] 的合成调用，把错误细节回灌给模型。
fn protocol_error_call(detail: &str) -> ToolCall {
    let message = format!(
        "工具调用无法解析：{detail}。请重新发送**完整、合法**的调用：一个 `<tool>` 块，\
包含 `<name>` 与合法的 `<params>` JSON，并闭合所有标签。"
    );
    ToolCall {
        id: format!("content_call_{}", Uuid::new_v4()),
        name: PROTOCOL_ERROR_TOOL.to_string(),
        arguments: serde_json::json!({ "message": message }).to_string(),
    }
}

/// 标签名配置（owned，便于随流式解析器装箱成 `'static`）。
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

    /// `parent` 能否直接包含 `child`：只有 `tool` 能包含 `name` / `params`。
    fn can_contain(&self, parent: &str, child: &str) -> bool {
        parent == self.tool.as_str() && (child == self.name.as_str() || child == self.params.as_str())
    }
}

/// 字符级解析状态机：支持任意分片（`feed` 逐字符喂入，状态跨调用保持）。
///
/// v1 只在最终态解析（`parse` 对完整文本跑一次），但按状态机实现，为后续流式解析留接缝。
struct TagParser {
    cfg: TagConfig,
    mode: Mode,
    /// 正在构建的标签 token（`<` 与 `>` 之间的内容）。
    token: String,
    /// 打开标签栈（含各自的内容缓冲）。
    stack: Vec<OpenTag>,
    /// 标签之外的散落文本（噪音回退用）。
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
    /// 解析中发现的**可纠正错误**（缺 name / 未闭合 params / 非法 JSON）。
    errors: Vec<String>,
    /// 流式可见增量（逐字符累积；`parse_tagged` 路径忽略之）。
    emitted: Vec<VisibleChunk>,
    /// 暂存的尾随空白：仅当后面紧跟非空白时才吐出，遇标签边界 / 结束时丢弃——
    /// 使流式可见文本与最终解析的 `trim()` 结果一致（不把标签间的换行当正文）。
    pending_ws: String,
}

struct OpenTag {
    name: String,
    buf: String,
}

enum Mode {
    Text,
    /// 刚读到 `<`，尚未确定它是否真的是标签开头（决定噪音回退还是进入标签）。
    TagOpen,
    Tag,
}

/// 可见增量通道：`Thinking`（→ ReasoningDelta）/ `Content`（→ ContentDelta）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum VisibleChannel {
    Thinking,
    Content,
}

fn parse_tagged(text: &str, cfg: TagConfig) -> ParsedDocument {
    if !starts_with_protocol_tag(text.trim_start(), &cfg) {
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
                // 注意：这里**不**因 `params_json_in_string` 而短路 `'<'`——否则字符串里
                // 未配对的引号会让 `</params>` 永远进不了标签解析（Step 5 的引号兜底依赖它）。
                // 字符串内部的 `<` 由 `resolve_tag` 的引号兜底还原为文本。
                if ch == '<' && !self.in_code_fence {
                    self.mode = Mode::TagOpen;
                    self.token.clear();
                } else {
                    self.push_text(ch);
                }
            }
            // `<` 之后的第一字符决定去向：
            //   - 合法标签起始字符（或 `/` 收尾）→ 进入标签 token；
            //   - `>` → 空标签 `<>`，跳过；
            //   - 又一个 `<` → 前一个 `<` 是噪音，重开标签；
            //   - 其它（空格、数字、标点…）→ `<` 是普通文本，整段回退。
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

    /// 把普通字符追加到「当前打开标签的内容」或「标签外缓冲」。
    fn push_text(&mut self, ch: char) {
        if self.stack.last().is_none_or(|tag| tag.name == self.cfg.description || tag.name == self.cfg.thinking) {
            if ch == '`' {
                self.backtick_run += 1;
                if self.backtick_run == 3 {
                    self.in_code_fence = !self.in_code_fence;
                }
            } else {
                self.backtick_run = 0;
            }
        }
        if self.stack.last().is_some_and(|tag| tag.name == self.cfg.params) {
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

    /// 把一段噪音（无法识别的标签）原样还原为文本。
    fn push_noise(&mut self, text: &str) {
        match self.stack.last_mut() {
            Some(top) => top.buf.push_str(text),
            None => self.outside.push_str(text),
        }
        for ch in text.chars() {
            self.emit_char(ch);
        }
    }

    /// 当前栈顶标签对应的**可见通道**（`None` = 屏蔽）。
    ///
    /// 标签外文本（栈为空）也返回 `None`：与最终解析「工具轮只保留 `<description>`」
    /// 对齐（Step 5）——`<tool>` 标签之间的换行 / 散落文字不再泄漏成 `ContentDelta`。
    fn visible_channel(&self) -> Option<VisibleChannel> {
        match self.stack.last() {
            None => None,
            Some(top) if top.name == self.cfg.thinking => Some(VisibleChannel::Thinking),
            Some(top) if top.name == self.cfg.description => Some(VisibleChannel::Content),
            Some(_) => None,
        }
    }

    /// 逐字符产出可见增量（屏蔽通道直接丢弃）。
    ///
    /// 空白先暂存（[`Self::pending_ws`]）：后面紧跟非空白才吐出，遇标签边界 / 结束时
    /// 丢弃——这样标签间的换行不会被当成正文，流式结果与最终 `trim()` 对齐。
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

    /// 把一段文本追加到指定可见通道（相邻同通道合并）。
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

    /// 取出累积的可见增量。
    fn take_visible(&mut self) -> Vec<VisibleChunk> {
        std::mem::take(&mut self.emitted)
    }

    fn resolve_tag(&mut self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // `<>` / `</>`：跳过。
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
        // 引号不配对兜底（Step 5）：`<params>` 内字符串引号未闭合时 `params_json_in_string`
        // 会卡在 true，`</params>` 会被当成字符串内容吞掉。栈顶是 params 且仍处于
        // 「字符串中」时���遇 `</params>` 先做 JSON 校验：能解析则正常闭合，否则整段还原为文本。
        if self.params_json_in_string
            && closing
            && name == self.cfg.params
            && self.stack.last().is_some_and(|tag| tag.name == self.cfg.params)
        {
            let buffer = self.stack.last().map(|tag| tag.buf.clone()).unwrap_or_default();
            if serde_json::from_str::<serde_json::Value>(buffer.trim()).is_err() {
                self.push_noise(&format!("<{raw}>"));
                return;
            }
        }
        // 非法 / 未知标签：整段还原为文本（例如 `a < b` 里的裸 `<`）。
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
        } else if closing || (name != self.cfg.thinking && name != self.cfg.description && name != self.cfg.tool) {
            self.push_noise(&format!("<{raw}>"));
            return;
        }
        // 真正的结构标签才丢弃边界空白；正文中的 XML 示例保留原有空白。
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
        // 兜底清理不合法的嵌套；协议标签在正文中已于 resolve_tag 回退成文本。
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
            // 找不到对应开标签：还原为文本。
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
            // 可见增量已在 `emit_char` 逐字符发出；弹栈时只丢弃缓冲，不补发。
            append_opt(&mut self.thinking, content);
        } else if top.name == self.cfg.description {
            // 反转义（与 `render_assistant` 的 escape_xml 对称，Step 6）。
            append_opt(&mut self.description, unescape_xml(&content));
        } else if top.name == self.cfg.name {
            self.tool_name = Some(content);
        } else if top.name == self.cfg.params {
            self.tool_params = Some(content);
        } else if top.name == self.cfg.tool {
            let name = self.tool_name.take().unwrap_or_default();
            let params = self.tool_params.take();
            // 解析错误不静默（Step 5）：缺 name / params 非法 JSON 记入 errors，
            // 由 `parse_content_response` 合成可纠正回灌，而不是带着垃圾参数执行。
            if name.is_empty() {
                self.errors.push("工具块缺少 `<name>`".to_string());
            } else {
                let arguments = match params {
                    Some(raw) if !raw.trim().is_empty() => {
                        if serde_json::from_str::<serde_json::Value>(raw.trim()).is_err() {
                            self.errors.push(format!("工具 `{name}` 的 `<params>` 不是合法 JSON"));
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
        // 结尾的暂存空白丢弃（与 `trim()` 对齐）。
        self.pending_ws.clear();
        // 输入结束仍在构建标签：未闭合的 `<...` / 孤立的 `<` 还原为文本。
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
                "工具块被截断（`<params>` 未闭合），无法执行".to_string()
            } else {
                "工具块未闭合（缺少 `</tool>`）".to_string()
            });
        }
        // 标签外散落文本统一丢弃（与流式 visible_channel 对齐，Step 5）。
        ParsedDocument {
            thinking: self.thinking.take(),
            description: self.description.take(),
            calls: std::mem::take(&mut self.calls),
            errors: std::mem::take(&mut self.errors),
        }
    }
}

/// [`TaggedToolProtocol`] 的**流式**解析器：把同一套字符级状态机包装成
/// [`ContentStreamParser`]，逐块喂入原始文本、只吐 `thinking` / `description` 增量。
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
            if opening_tags.iter().any(|tag| tag == trimmed || trimmed.starts_with(tag)) {
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
        // 冲刷状态机（未闭合标签 / 孤立 `<` 还原为噪音，可能带出可见增量）。
        self.parser.finish();
        self.parser.take_visible()
    }
}

/// 转义 XML 元字符，保证回放文本里的 `<` / `&` 不会被当成协议标签（Step 6）。
/// 与解析侧 [`unescape_xml`] 严格互逆。
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 反转义 [`escape_xml`] 写出的实体（顺序与转义相反）。
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

/// 标签名合法性：首字符是 `NameStartChar`、其余是 `NameChar`（W3C XML 的务实子集）。
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if is_name_start(first) => chars.all(is_name_char),
        _ => false,
    }
}

/// W3C XML `NameStartChar` 的务实子集：字母 / `_` / `:`。
fn is_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == ':'
}

/// W3C XML `NameChar` 的务实子集：`NameStartChar` + 数字 / `-` / `.`。
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
            "<thinking>想一下</thinking>\n<description>我来看看</description>\n<tool>\n<name>read_file</name>\n<params>{\"path\":\"a.rs\"}</params>\n</tool>",
        );
        assert_eq!(doc.thinking.as_deref(), Some("想一下"));
        assert_eq!(doc.description.as_deref(), Some("我来看看"));
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "read_file".into(),
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
        let Message::Assistant { tool_calls: first, .. } = parse_content_response(&protocol(), response()) else { panic!("expected assistant") };
        let Message::Assistant { tool_calls: second, .. } = parse_content_response(&protocol(), response()) else { panic!("expected assistant") };
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
                    "<thinking>检查路径</thinking><description>我先读取文件</description><tool><name>read_file</name><params>{\"path\":\"a.txt\"}</params></tool>"
                }]
            }]
        })).unwrap();
        let message = parse_content_response(&protocol(), response.message);
        let Message::Assistant { content, tool_calls, .. } = message else { panic!("expected assistant") };
        assert_eq!(content.as_deref(), Some("我先读取文件"));
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read_file");
        assert_eq!(tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn description_only_has_no_calls() {
        let doc = protocol().parse("<description>直接回答，不调工具</description>");
        assert_eq!(doc.description.as_deref(), Some("直接回答，不调工具"));
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn nested_protocol_tag_is_literal_until_parent_closes() {
        let doc = protocol().parse("<thinking>推理中<description>回答</description>");
        assert_eq!(doc.thinking.as_deref(), Some("推理中<description>回答</description>"));
        assert!(doc.description.is_none());
    }

    #[test]
    fn fenced_xml_example_is_not_a_tool_call() {
        let example = "示例：\n```xml\n<tool><name>bash</name><params>{}</params></tool>\n```";
        let doc = protocol().parse(example);
        assert_eq!(doc.description.as_deref(), Some(example));
        assert!(doc.calls.is_empty());

        let wrapped = format!("<description>{example}</description>");
        let doc = protocol().parse(&wrapped);
        assert_eq!(doc.description.as_deref(), Some(example));
        assert!(doc.calls.is_empty());
        let (_, streamed) = collect_visible(&protocol(), &[&wrapped]);
        assert_eq!(streamed, example);

        let outside = format!("<description>示例</description>\n```xml\n<tool><name>bash</name><params>{{}}</params></tool>\n```");
        assert!(protocol().parse(&outside).calls.is_empty());

        let inline = "<description>调用示例：\n<tool><name>bash</name><params>{}</params></tool></description>";
        assert!(protocol().parse(inline).calls.is_empty());
        let (_, streamed) = collect_visible(&protocol(), &[inline]);
        assert_eq!(streamed, "调用示例：\n<tool><name>bash</name><params>{}</params></tool>");
    }

    #[test]
    fn unclosed_tool_block_does_not_execute() {
        let doc = protocol().parse("<tool><name>bash</name><params>{}</params>");
        assert!(doc.calls.is_empty());
    }

    #[test]
    fn truncated_params_records_error() {
        // 被 max_tokens 截断：`<params>` 未闭合 → 不执行，且记入 errors（Step 5）。
        let doc = protocol().parse("<tool><name>bash</name><params>{\"command\":\"ls");
        assert!(doc.calls.is_empty());
        assert!(!doc.errors.is_empty(), "应记录截断错误: {doc:?}");
    }

    #[test]
    fn missing_name_records_error() {
        let doc = protocol().parse("<tool><params>{}</params></tool>");
        assert!(doc.calls.is_empty());
        assert!(doc.errors.iter().any(|e| e.contains("name")), "应记录缺 name: {doc:?}");
    }

    #[test]
    fn invalid_params_json_records_error() {
        let doc = protocol().parse("<tool><name>bash</name><params>{not json}</params></tool>");
        assert!(!doc.errors.is_empty(), "应记录非法 JSON: {doc:?}");
    }

    #[test]
    fn unmatched_quote_in_params_falls_back_to_text() {
        // 字符串引号未闭合时 `</params>` 不应被吞掉；非法 JSON → 还原为文本 + 记错误。
        let doc = protocol().parse("<tool><name>bash</name><params>{\"command\":\"ls}</params></tool>");
        assert!(doc.calls.is_empty(), "非法 JSON 不应执行: {doc:?}");
        assert!(!doc.errors.is_empty(), "应记录非法 JSON: {doc:?}");
    }

    #[test]
    fn parse_error_produces_protocol_recovery_call() {
        // 解析错误 → 合成一条 `__protocol__` 调用回灌，不执行真实工具（Step 5）。
        let message = Message::Assistant {
            content: Some("<tool><params>{}</params></tool>".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let Message::Assistant { tool_calls, .. } = parse_content_response(&protocol(), message) else {
            panic!("expected assistant");
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, crate::tool::PROTOCOL_ERROR_TOOL);
        assert!(tool_calls[0].arguments.contains("message"));
    }

    #[test]
    fn native_reasoning_wins_over_tagged_thinking() {
        // 思考来源优先级：原生 reasoning 优先，`<thinking>` 仅补空（Step 5）。
        let message = Message::Assistant {
            content: Some("<thinking>标签思考</thinking><description>答</description>".into()),
            reasoning_content: Some("原生思考".into()),
            tool_calls: Vec::new(),
        };
        let Message::Assistant { reasoning_content, .. } = parse_content_response(&protocol(), message) else {
            panic!("expected assistant");
        };
        assert_eq!(reasoning_content.as_deref(), Some("原生思考"));

        // 原生为空时采用 `<thinking>`。
        let message = Message::Assistant {
            content: Some("<thinking>标签思考</thinking><description>答</description>".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let Message::Assistant { reasoning_content, .. } = parse_content_response(&protocol(), message) else {
            panic!("expected assistant");
        };
        assert_eq!(reasoning_content.as_deref(), Some("标签思考"));
    }

    #[test]
    fn stream_discards_text_outside_tags() {
        // 标签外散落文本统一丢弃（与最终解析对齐，Step 5）。
        let protocol = protocol();
        let (thinking, content) = collect_visible(
            &protocol,
            &["<description>答</description>\n", "some stray text\n", "<tool><name>bash</name><params>{}</params></tool>"],
        );
        assert_eq!(thinking, "");
        assert_eq!(content, "答");
    }

    #[test]
    fn bare_angle_bracket_falls_back_to_text() {
        // `a < b` 不是合法标签，应还原为文本进入 description。
        let doc = protocol().parse("<description>比较 a < b 的大小</description>");
        assert_eq!(doc.description.as_deref(), Some("比较 a < b 的大小"));
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
        // 逐字符喂入（模拟任意分片）与整体解析结果一致。
        let text = "<tool><name>x</name><params>{}</params></tool>";
        let proto = protocol();
        let whole = parse_tagged(text, proto.tags());
        let mut parser = TagParser::new(proto.tags());
        for ch in text.chars() {
            parser.feed(ch);
        }
        assert_eq!(parser.finish(), whole);
    }

    /// 把流式解析器吐出的可见增量拼成 (thinking, content) 两段文本。
    fn collect_visible(protocol: &TaggedToolProtocol, pieces: &[&str]) -> (String, String) {
        let mut parser = protocol.stream_parser();
        let mut thinking = String::new();
        let mut content = String::new();
        let mut absorb = |chunks: Vec<VisibleChunk>,
                          thinking: &mut String,
                          content: &mut String| {
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
            &["<thinking>想一想</thinking>\n<description>这就做</description>\n<tool><name>read_file</name><params>{\"path\":\"a.rs\"}</params></tool>"],
        );
        // thinking / description 增量可见；`<tool>` 块完全屏蔽。
        assert_eq!(thinking, "想一想");
        assert_eq!(content, "这就做");
        assert!(!content.contains("read_file"));
        assert!(!content.contains("tool"));
    }

    #[test]
    fn stream_parser_is_split_insensitive() {
        // 同一文档任意分片喂入，可见增量拼接结果一致（含工具块屏蔽）。
        let protocol = protocol();
        let doc = "<thinking>想</thinking><description>答</description><tool><name>bash</name><params>{}</params></tool>";
        let one_shot = collect_visible(&protocol, &[doc]);
        let per_char: Vec<String> = doc.chars().map(|c| c.to_string()).collect();
        let per_char: Vec<&str> = per_char.iter().map(String::as_str).collect();
        let split = collect_visible(&protocol, &per_char);
        assert_eq!(one_shot, split);
        assert_eq!(one_shot.1, "答");
        assert_eq!(one_shot.0, "想");
    }

    #[test]
    fn stream_parser_passes_through_plain_text() {
        // 纯文本（无标签）直接作为展示文本流出，不被吞掉。
        let protocol = protocol();
        let (thinking, content) = collect_visible(&protocol, &["你好，", "世界"]);
        assert_eq!(thinking, "");
        assert_eq!(content, "你好，世界");
    }

    #[test]
    fn default_stream_parser_swallows_everything() {
        // 未实现流式解析的协议：默认空解析器不外泄任何原始文本（保守安全默认）。
        struct Bare;
        impl ContentToolProtocol for Bare {
            fn render_instructions(&self, _tools: &[&ToolDefinition]) -> String {
                String::new()
            }
            fn parse(&self, _text: &str) -> ParsedDocument {
                ParsedDocument::default()
            }
            fn render_assistant(&self, _t: Option<&str>, _c: Option<&str>, _calls: &[ToolCall]) -> String {
                String::new()
            }
            fn render_result(&self, _call: &ToolCall, _output: &str) -> Message {
                Message::User { content: String::new() }
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
                assert!(content.contains("\"tool\":\"bash\""));
                assert!(content.ends_with("hello"));
            }
            other => panic!("应为 User 回灌消息，实际: {other:?}"),
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
        // thinking 不再回放（Step 6）：历史 reasoning 每轮带上会膨胀上下文。
        let text = proto.render_assistant(Some("想一下"), Some("看一下目录"), &calls);
        assert!(!text.contains("想一下"), "不应回放 thinking: {text}");
        let doc = proto.parse(&text);
        assert_eq!(doc.thinking.as_deref(), None);
        assert_eq!(doc.description.as_deref(), Some("看一下目录"));
        assert_eq!(
            doc.calls,
            vec![ParsedToolCall {
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }]
        );
    }

    /// 回放对**正文**做 XML 转义（参数保持 JSON 原文），保证含 `<` 的文本往返解析无损（Step 6）。
    #[test]
    fn render_assistant_escapes_content_and_round_trips() {
        let proto = protocol();
        let content = "比较 a < b 且 x & y";
        let args = r#"{"command":"echo '<tool>'"}"#;
        let calls = vec![ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: args.into(),
        }];
        let text = proto.render_assistant(None, Some(content), &calls);
        assert!(text.contains("&lt;"), "应转义正文里的 `<`: {text}");
        let doc = proto.parse(&text);
        assert_eq!(doc.description.as_deref(), Some(content));
        assert_eq!(doc.calls.len(), 1);
        assert_eq!(doc.calls[0].arguments, args);
    }

    /// 回放保留标签：普通 assistant（无 tool_calls）也重渲染成协议原文，
    /// 而不是原样透传剥壳后的纯文本——否则 `<description>` 标签会丢，KV 前缀逐轮漂移。
    #[test]
    fn encode_keeps_tags_on_plain_assistant() {
        let proto = protocol();
        let messages = vec![
            Message::User {
                content: "你好".into(),
            },
            Message::Assistant {
                content: Some("我在".into()),
                reasoning_content: Some("想一下".into()),
                tool_calls: Vec::new(),
            },
        ];
        let out = encode_messages_for_content(&messages, &[], &proto);
        // user 原样保留。
        assert!(matches!(&out[0], Message::User { content } if content == "你好"));
        // assistant 被重渲染成带标签原文（标签必须出现）。
        let Message::Assistant { content, tool_calls, .. } = &out[1] else {
            panic!("expected assistant");
        };
        let text = content.as_deref().unwrap();
        // 不回放 thinking（Step 6），但仍保留 description 标签。
        assert!(!text.contains("<thinking>"), "不应回放 thinking: {text}");
        assert!(text.contains("<description>我在</description>"), "description 标签: {text}");
        assert!(tool_calls.is_empty());
    }

    /// 回放保留标签：带工具调用的 assistant 与后续 tool 结果都被正确重渲染。
    #[test]
    fn encode_rerenders_tool_call_and_result() {
        let proto = protocol();
        let messages = vec![
            Message::Assistant {
                content: Some("看一下".into()),
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
        assert!(text.contains("<tool>"), "tool 标签: {text}");
        assert!(text.contains("<name>bash</name>"), "工具名: {text}");
        // tool 结果按模板渲染（默认模板是 `{"tool":"{tool}"}` + 换行 + 输出）。
        let Message::User { content } = &out[1] else {
            panic!("expected user (rendered tool result)");
        };
        assert!(content.contains("{\"tool\":\"bash\"}"), "回灌头: {content}");
        assert!(content.contains("file.txt"), "回灌体: {content}");
    }

    /// 同一轮 assistant 并发调用多个工具 → runtime 落成多条连续 `Message::Tool`，
    /// 编码时必须**合并成一条 user 消息**（`\n\n` 分隔），否则 Anthropic 等要求
    /// 角色交替的协议会因连续多条 user 而报错（docs/tool-calling-refactor.md Step 4）。
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
                        name: "read_file".into(),
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
        // assistant + 一条合并的 user（而不是两条 user）。
        assert_eq!(out.len(), 2, "多工具结果应合并，实际输出: {out:?}");
        assert!(matches!(&out[0], Message::Assistant { .. }));
        let Message::User { content } = &out[1] else {
            panic!("expected single merged user, got {:?}", out[1]);
        };
        // 两块结果都在，且按模板各自带头。
        assert!(content.contains("{\"tool\":\"bash\"}"), "缺 bash 块: {content}");
        assert!(content.contains("file.txt"), "缺 bash 输出: {content}");
        assert!(content.contains("{\"tool\":\"read_file\"}"), "缺 read_file 块: {content}");
        assert!(content.contains("fn main() {}"), "缺 read_file 输出: {content}");
        // 两块之间用空行分隔。
        assert_eq!(content.matches("{\"tool\":").count(), 2, "应有两块: {content}");
    }

    #[test]
    fn render_instructions_lists_tools() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "执行命令".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let text = protocol().render_instructions(&[&def]);
        assert!(text.contains("bash"));
        assert!(text.contains("执行命令"));
        assert!(text.contains("<tool>"));
        // 仿 figmaagent 的四节结构（契约锁死，防静默回归）。
        assert!(text.contains("# 工具调用协议"), "缺运行机制节: {text}");
        assert!(text.contains("# 输出格式（务必严格遵守）"), "缺输出格式节: {text}");
        assert!(text.contains("# 可用工具"), "缺工具目录节: {text}");
        assert!(text.contains("# 工具结果回灌"), "缺回灌识别节: {text}");
        // 参数渲染成紧凑文档（而非原始 JSON Schema）。
        assert!(text.contains("参数："), "缺参数文档: {text}");
        // 契约必须覆盖「无需工具、直接纯文本」的分支（否则模型可能强行发工具块）。
        assert!(
            text.contains("不需要工具时直接输出纯文本"),
            "缺『无需工具时纯文本』契约: {text}"
        );
        assert!(
            text.contains("不需要工具时**：直接输出纯文本回答即可"),
            "缺纯文本约束条目: {text}"
        );
    }

    /// JSON Schema -> compact doc: type / optional / comment / constraint / enum / nesting.
    #[test]
    fn schema_doc_renders_compact_readable() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "timeout": { "type": "integer", "minimum": 1, "description": "超时秒数" },
                "mode": { "enum": ["fast", "safe"] }
            }
        });
        let doc = render_schema_doc(&schema);
        assert!(doc.contains("command: string"), "required field must have no ?: {doc}");
        assert!(doc.contains("// 要执行的命令"), "comment: {doc}");
        assert!(doc.contains("timeout?: integer (>=1)"), "optional+constraint: {doc}");
        assert!(doc.contains("\"fast\" | \"safe\""), "enum: {doc}");
    }
}
