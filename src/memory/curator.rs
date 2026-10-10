//! 会话结束后的增量 curator（`docs/memory.md` §5）。
//!
//! 写入路径不靠"AI 主动调工具"（那看模型心情），而是**会话结束时**由 curator 读
//! 轨迹、产出尽可能小的 diff（新条目 / 取代关系），再过一道**确定性自检**合入。
//!
//! V1 的三条取舍（§5.3）：
//! - **同模型**：用 [`Agent::complete`]（一次性、无工具、非流式、不碰会话日志）。
//!   异源审核（独立模型）留到 V2。
//! - **确定性自检**：不靠 LLM，只查 schema / `source` / `supersedes` / 时间 / 链接。
//!   **不通过就不合入，绝不默认放行**。
//! - **诚实降级**：只读探测（§5.4）V1 不做，退化为纯轨迹 curation。
//!
//! 边界声明（照抄论文的 distiller 边界）：轨迹是**部分证据**、不是 ground truth——
//! 一次通过不自动验证每个中间假设。这条写进给模型的提示词里。
//!
//! **V2 新增定期整理（睡眠学习，`docs/memory.md` §5.2 / §6）**：[`consolidate`] 全量扫描
//! 记忆库，让模型产出 merge / supersede / requalify 操作，逐条经**确定性自检**后执行，
//! 并**回原始证据核查**（把条目 `source` 指向的 `sessions/*.jsonl` 片段喂进提示词）。
//! 冲突**不强行收敛**——条件不同就 `requalify` 补 `scope`，证据不足就保留原样。

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use shirley_agent_sdk::{Agent, Message};

use super::format::{Confidence, Entry, EntryStatus, EntryType, MemoryError};
use super::store::MemoryStore;

/// curator 的错误。
#[derive(Debug, thiserror::Error)]
pub enum CuratorError {
    /// 调模型失败（收敛成字符串，避免 curator 依赖 SDK 的错误类型）。
    #[error("curator model call failed: {0}")]
    Model(String),
    /// 模型输出不是合法 JSON。
    #[error("curator output not parseable: {0}")]
    Parse(String),
    /// 落盘失败。
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

/// 一次 curation 的结果。
#[derive(Debug, Default)]
pub struct CurateOutcome {
    /// 成功写入的条目 id。
    pub written: Vec<String>,
    /// 被确定性自检拒绝的候选（id 可能缺失）。
    pub rejected: Vec<Rejected>,
    /// 模型没产出任何候选（正常：这轮没有可沉淀的东西）。
    pub empty: bool,
}

/// 一条被拒绝的候选及原因。
///
/// 供上层观测（`spawn_curation` 会打印计数）；V1 尚无消费方，字段为诊断保留。
#[derive(Debug)]
#[allow(dead_code)]
pub struct Rejected {
    pub id: Option<String>,
    pub reason: String,
}

/// 模型输出的候选条目（与 [`Entry`] 解耦：模型只提供这些字段，其余由 curator 补）。
#[derive(Debug, Clone, Deserialize)]
pub struct Candidate {
    pub id: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub subject: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub source: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// 从模型文本里抽出候选数组（容忍 ```json 代码围栏与前后废话）。
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, CuratorError> {
    let start = text
        .find('[')
        .ok_or_else(|| CuratorError::Parse("no JSON array found".into()))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| CuratorError::Parse("no closing `]`".into()))?;
    if end < start {
        return Err(CuratorError::Parse("malformed JSON array".into()));
    }
    serde_json::from_str(&text[start..=end])
        .map_err(|err| CuratorError::Parse(err.to_string()))
}

/// 确定性自检 + 补全默认字段。通过返回 [`Entry`]，否则返回拒绝原因。
///
/// 自检项（§5.3，不靠 LLM）：
/// - `id` / `subject` / `body` 非空，`id` 是 slug；
/// - 时间字段是 `YYYY-MM-DD`；
/// - `source` 非空（**stamp**：轨迹本就来自该会话，缺省时补上已知的 `session_ref`，
///   这是补全真实来源、不是编造证据）；给了 `sessions_dir` 时还校验源文件存在；
/// - `supersedes` 指向的 id 确实存在。
pub fn vet(
    candidate: Candidate,
    existing: &HashMap<String, Entry>,
    today: &str,
    session_ref: Option<&str>,
    sessions_dir: Option<&Path>,
) -> Result<Entry, String> {
    let id = candidate.id.trim();
    if id.is_empty() {
        return Err("empty id".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("id `{id}` is not a slug"));
    }
    if candidate.subject.trim().is_empty() {
        return Err("empty subject".into());
    }
    if candidate.body.trim().is_empty() {
        return Err("empty body".into());
    }

    let created_at = candidate
        .created_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(today)
        .to_string();
    if !is_date(&created_at) {
        return Err(format!("created_at `{created_at}` is not YYYY-MM-DD"));
    }
    if let Some(valid_from) = candidate.valid_from.as_deref()
        && !is_date(valid_from.trim())
    {
        return Err(format!("valid_from `{valid_from}` is not YYYY-MM-DD"));
    }

    let mut source: Vec<String> = candidate
        .source
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if source.is_empty()
        && let Some(session_ref) = session_ref
    {
        source.push(session_ref.to_string());
    }
    if source.is_empty() {
        return Err("missing source".into());
    }
    if let Some(dir) = sessions_dir {
        for ref_ in &source {
            let file = ref_.split('#').next().unwrap_or(ref_);
            if !dir.join(file).exists() {
                return Err(format!("source `{ref_}` does not resolve to an existing file"));
            }
        }
    }

    if let Some(supersedes) = candidate.supersedes.as_deref() {
        let supersedes = supersedes.trim();
        if supersedes.is_empty() {
            return Err("empty supersedes".into());
        }
        if supersedes == id {
            return Err("entry supersedes itself".into());
        }
        if !existing.contains_key(supersedes) {
            return Err(format!("supersedes `{supersedes}` does not exist"));
        }
    }

    Ok(Entry {
        id: id.to_string(),
        entry_type: candidate.entry_type,
        subject: candidate.subject.trim().to_string(),
        created_at,
        valid_from: candidate
            .valid_from
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        supersedes: candidate
            .supersedes
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        status: EntryStatus::Active,
        confidence: candidate.confidence.unwrap_or(Confidence::Medium),
        scope: candidate
            .scope
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        utility: None,
        usage_count: None,
        source,
        body: candidate.body.trim().to_string(),
    })
}

fn is_date(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// 把一条消息渲染成 curator 可读的一行（跳过 system / 压缩摘要——不是对话内容）。
pub fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            Message::User { content } => {
                out.push_str("## User\n");
                out.push_str(content.trim());
                out.push_str("\n\n");
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(content) = content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("## Assistant\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
                for call in tool_calls {
                    out.push_str(&format!("## Tool call: {}\n{}\n\n", call.name, call.arguments));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                out.push_str(&format!("## Tool result ({tool_call_id})\n"));
                if let Some(content) = content.as_deref() {
                    out.push_str(content.trim());
                }
                out.push('\n');
                out.push('\n');
            }
            Message::System { .. } | Message::ContextSummary { .. } => {}
        }
    }
    out
}

/// 组装 curator 提示词：边界声明 + 现有相关记忆（摘要）+ 轨迹 + 输出格式。
pub fn build_prompt(transcript: &str, existing: &[(String, Entry)], session_ref: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "你是记忆整理器（curator）。读下面这段对话轨迹，产出**尽可能小**的记忆条目增量。\n\n\
         **边界**：轨迹只是对环境的一次局部观察，是部分证据、不是 ground truth——\
         一次成功不自动验证每个中间假设。只沉淀**跨会话仍然成立**的用户事实 / 偏好 / 事件 / 程序；\
         不确定就不要写（或把 confidence 设为 low）。不要推演轨迹里没有的东西。\n\n\
         只输出一个 JSON 数组（可为空 `[]`），每个元素字段：\n\
         - `id`: 稳定 slug（小写字母数字与 `-`），如 `pref-rust-error-style`\n\
         - `type`: `preference` | `fact` | `event` | `procedure`\n\
         - `subject`: 主题（供检索），如 `rust-error-handling`\n\
         - `created_at`: `YYYY-MM-DD`（可省，默认今天）\n\
         - `valid_from`: `YYYY-MM-DD`（可省）\n\
         - `supersedes`: 若取代已有条目，填它的 id（否则省略）\n\
         - `confidence`: `high` | `medium` | `low`\n\
         - `scope`: 适用场景限定（可省）\n\
         - `source`: 证据引用数组，**必填**，格式 `<会话文件>#turn:N`，本次会话填 ",
    );
    prompt.push_str(&format!("`{session_ref}#turn:N`"));
    prompt.push_str("\n- `body`: 正文（Markdown，一句话讲清）\n\n");
    prompt.push_str("不要删除历史：要改旧偏好就新建条目并 `supersedes` 旧 id。\n\n");

    if !existing.is_empty() {
        prompt.push_str("## 现有相关记忆（避免重复；如需取代用其 id）\n");
        for (_, entry) in existing {
            prompt.push_str(&format!(
                "- [{}] {} — {}\n",
                entry.id,
                entry.subject,
                entry.summary()
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str("## 对话轨迹\n");
    prompt.push_str(transcript);
    prompt
}

/// 会话结束触发一次增量 curation。
///
/// - `agent`：用来调模型（同模型，一次性补全）；
/// - `store`：记忆存储（读现有条目、写新条目、重建索引）；
/// - `messages`：本会话全量消息（curator 的输入轨迹）；
/// - `session_ref`：本会话在 `sessions/` 下的相对文件名（用作 `source` 前缀）。
pub async fn curate(
    agent: &Agent,
    store: &MemoryStore,
    messages: &[Message],
    session_ref: &str,
    sessions_dir: Option<&Path>,
) -> Result<CurateOutcome, CuratorError> {
    let existing_pairs: Vec<(String, Entry)> = store
        .list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    let existing_by_id: HashMap<String, Entry> = existing_pairs
        .iter()
        .map(|(_, entry)| (entry.id.clone(), entry.clone()))
        .collect();

    let transcript = render_transcript(messages);
    if transcript.trim().is_empty() {
        return Ok(CurateOutcome {
            empty: true,
            ..Default::default()
        });
    }
    let prompt = build_prompt(&transcript, &existing_pairs, session_ref);

    let raw = agent
        .complete(&prompt)
        .await
        .map_err(|err| CuratorError::Model(err.to_string()))?;
    let candidates = parse_candidates(&raw)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut outcome = CurateOutcome {
        empty: candidates.is_empty(),
        ..Default::default()
    };

    for candidate in candidates {
        let candidate_id = Some(candidate.id.clone());
        match vet(
            candidate,
            &existing_by_id,
            &today,
            Some(session_ref),
            sessions_dir,
        ) {
            Ok(entry) => {
                // 取代：旧条目保留，只改状态（`docs/memory.md` 决策 4：永不删历史）。
                if let Some(old_id) = entry.supersedes.clone()
                    && let Some(old) = existing_by_id.get(&old_id)
                {
                    let mut old = old.clone();
                    if old.status != EntryStatus::Superseded {
                        old.status = EntryStatus::Superseded;
                        store.write_entry(&old)?;
                    }
                }
                store.write_entry(&entry)?;
                outcome.written.push(entry.id.clone());
            }
            Err(reason) => outcome.rejected.push(Rejected {
                id: candidate_id,
                reason,
            }),
        }
    }

    rebuild_index(store)?;
    rebuild_core(store)?;
    Ok(outcome)
}

/// 用当前全部条目重建 `index.md`（curator 合入后调用）。
pub fn rebuild_index(store: &MemoryStore) -> Result<(), MemoryError> {
    let entries: Vec<(String, Entry)> = store
        .list_entries()?
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    super::index::write_index(store, &entries)
}

/// `core.md` 常驻层硬上限（字符；≈ 500 token，`docs/memory.md` §4.3）。
pub const MAX_CORE_CHARS: usize = 2000;

/// 用**活跃条目**确定性重建 `core.md`（常驻层：画像 + 明确偏好 + 活跃项目）。
///
/// **不靠 LLM**：常驻层必须小而稳定（前缀缓存友好），由程序按确定规则从条目投影即可，
/// 不值得为它多一次模型调用、也不该让它随模型心情漂移。规则：
///
/// - 只取 `status: active` 的条目（`superseded` / `unconfirmed` 不进常驻层）；
/// - 顺序固定：`preference` → `fact` → `procedure` → `event`（画像 / 偏好在前）；
/// - 每条一行 `- [type] <摘要>`，同类型内按时间新→旧、再按 id 稳定；
/// - 超过 [`MAX_CORE_CHARS`] 截断并显式标注（防常驻层自己膨胀，`docs/memory.md` §4.3）。
///
/// 空库时删除旧 `core.md`（避免残留过时画像被继续注入）。
pub fn rebuild_core(store: &MemoryStore) -> Result<(), MemoryError> {
    let mut entries: Vec<Entry> = store
        .list_entries()?
        .into_iter()
        .map(|(_, entry)| entry)
        .filter(|entry| entry.status == EntryStatus::Active)
        .collect();

    let path = store.primary_root().join(super::store::CORE_FILE);
    if entries.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }

    // 固定类型顺序（画像 / 偏好在前，事件在后），组内时间新→旧、再按 id。
    let rank = |entry_type: EntryType| match entry_type {
        EntryType::Preference => 0,
        EntryType::Fact => 1,
        EntryType::Procedure => 2,
        EntryType::Event => 3,
    };
    entries.sort_by(|a, b| {
        rank(a.entry_type)
            .cmp(&rank(b.entry_type))
            .then_with(|| b.timeline_date().cmp(a.timeline_date()))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut text = String::from("# Memory Core\n\n> 由程序维护（活跃条目投影）；勿手改。\n\n");
    let mut truncated = false;
    for entry in &entries {
        let line = format!(
            "- [{}] {}\n",
            super::provider::type_label(entry.entry_type),
            entry.summary()
        );
        if text.chars().count() + line.chars().count() > MAX_CORE_CHARS {
            truncated = true;
            break;
        }
        text.push_str(&line);
    }
    if truncated {
        text.push_str("\n_(truncated; see index.md for the full list)_\n");
    }

    store.write_core(&text)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// V2-B：定期整理（睡眠学习，`docs/memory.md` §5.2 / §6）
// ---------------------------------------------------------------------------

/// 原始证据片段注入上限（防证据自己垄断提示词）。
pub const MAX_EVIDENCE_CHARS: usize = 6000;

/// 一次定期整理的确定性结果。
#[derive(Debug, Default)]
pub struct ConsolidateOutcome {
    /// 执行成功的 merge 操作数。
    pub merged: usize,
    /// 被置为 superseded 的旧条目数。
    pub superseded: usize,
    /// 执行成功的 requalify 操作数。
    pub requalified: usize,
    /// 被自检拒绝的操作。
    pub rejected: Vec<Rejected>,
    /// 无可整理内容（条目不足或模型没产出操作）。
    pub empty: bool,
}

/// 一条整理操作（模型提议，程序自检后执行）。**永不删除历史**——只置 `superseded`。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ConsolidateOp {
    /// 把 `sources` 合并成 `target` 一条新条目，旧条目全部置 superseded。
    Merge {
        target: Candidate,
        sources: Vec<String>,
    },
    /// 用 `replacement` 取代 `id`（`id` 置 superseded）。
    Supersede {
        id: String,
        replacement: Candidate,
    },
    /// 只改一条条目的适用场景 / 正文（补 qualification），不新建条目。
    Requalify {
        id: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        body: Option<String>,
    },
}

/// 从模型文本里抽出整理操作数组（容忍 ```json 围栏与前后废话）。
pub fn parse_consolidate_ops(text: &str) -> Result<Vec<ConsolidateOp>, CuratorError> {
    let start = text
        .find('[')
        .ok_or_else(|| CuratorError::Parse("no JSON array found".into()))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| CuratorError::Parse("no closing `]`".into()))?;
    if end < start {
        return Err(CuratorError::Parse("malformed JSON array".into()));
    }
    serde_json::from_str(&text[start..=end]).map_err(|err| CuratorError::Parse(err.to_string()))
}

/// 回原始证据核查（§5.2）：把每条条目 `source` 指向的会话片段摘出来，供整理提示词。
///
/// `source` 形如 `<会话文件>#turn:N`：给了 `#turn:N` 就取该行（1-based），否则取整文件
/// 截断。文件缺失记 `[missing evidence]`——**不编造证据**。
pub fn collect_evidence(entries: &[(String, Entry)], sessions_dir: &Path, budget: usize) -> String {
    let mut out = String::new();
    for (_, entry) in entries {
        if out.chars().count() >= budget {
            break;
        }
        out.push_str(&format!("### {}\n", entry.id));
        for ref_ in &entry.source {
            if out.chars().count() >= budget {
                break;
            }
            out.push_str(&format!("- {}: ", ref_));
            out.push_str(&evidence_snippet(ref_, sessions_dir));
            out.push('\n');
        }
    }
    out
}

/// 摘一条证据引用对应的会话片段。
fn evidence_snippet(ref_: &str, sessions_dir: &Path) -> String {
    let mut parts = ref_.splitn(2, '#');
    let file = parts.next().unwrap_or(ref_).trim();
    let turn = parts
        .next()
        .and_then(|t| t.strip_prefix("turn:"))
        .and_then(|n| n.trim().parse::<usize>().ok());
    let path = sessions_dir.join(file);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return "[missing evidence]".into();
    };
    match turn {
        Some(n) if n >= 1 => text
            .lines()
            .nth(n - 1)
            .unwrap_or("[turn out of range]")
            .to_string(),
        _ => truncate(&text, 800),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// 组装整理提示词：边界声明 + 全部条目 + 原始证据 + 输出格式。
pub fn build_consolidate_prompt(entries: &[(String, Entry)], evidence: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "你是记忆整理器（consolidator）。下面是**全部**记忆条目及其原始证据片段。\n\n\
         **边界**：证据只是对环境的一次局部观察，是部分证据、不是 ground truth。只在确有依据时整理；\
         证据不足就**保留原样**，不要强行收敛。\n\n\
         任务：\n\
         1. **去重 / 合并**：语义重复或过碎的条目 → `merge` 成一条；\n\
         2. **去旧 / 取代**：被新条目取代的旧条目 → `supersede`；\n\
         3. **冲突场景限定**：两条都成立但条件不同 → `requalify` 给它们补 `scope`，**不要二选一**。\n\n\
         **永不删除历史**（只把旧条目置 superseded）。只输出 JSON 数组（可为空 `[]`），每项是下列之一：\n\
         - {\"op\":\"merge\",\"sources\":[\"id1\",\"id2\"],\"target\":{...条目字段...}}\n\
         - {\"op\":\"supersede\",\"id\":\"old-id\",\"replacement\":{...条目字段...}}\n\
         - {\"op\":\"requalify\",\"id\":\"id\",\"scope\":\"适用场景\",\"body\":\"可选新正文\"}\n\
         条目字段同增量 curator：id / type / subject / created_at / valid_from / confidence / scope / source / body。\n\n",
    );

    prompt.push_str("## 现有条目\n");
    for (_, entry) in entries {
        prompt.push_str(&format!(
            "- [{}] ({}) {} — {}（{}）\n",
            entry.id,
            entry.entry_type,
            entry.subject,
            entry.summary(),
            entry.timeline_date()
        ));
    }
    prompt.push('\n');

    if !evidence.trim().is_empty() {
        prompt.push_str("## 原始证据片段\n");
        prompt.push_str(evidence);
        prompt.push('\n');
    }
    prompt
}

/// 确定性执行整理操作（自检不通过就拒绝，绝不默认放行）。
///
/// 与 [`vet`] 同一原则：模型只提议，程序裁决。会写盘（新条目 / 改状态 / 改 scope），
/// 但不重建索引（由 [`consolidate`] 统一做）。
pub fn apply_consolidate_ops(
    store: &MemoryStore,
    ops: Vec<ConsolidateOp>,
    existing: &HashMap<String, Entry>,
    today: &str,
) -> Result<ConsolidateOutcome, CuratorError> {
    let mut working = existing.clone();
    let mut outcome = ConsolidateOutcome {
        empty: ops.is_empty(),
        ..Default::default()
    };

    for op in ops {
        match op {
            ConsolidateOp::Merge { mut target, sources } => {
                let target_id = target.id.clone();
                let sources: Vec<String> = sources
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if sources.is_empty() {
                    outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason: "merge without sources".into(),
                    });
                    continue;
                }
                if let Some(missing) = sources.iter().find(|id| !working.contains_key(*id)) {
                    outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason: format!("merge references unknown source `{missing}`"),
                    });
                    continue;
                }
                // 证据继承：新条目没给 source 就并集旧条目的 source（真实来源，不编造）。
                if target.source.is_empty() {
                    target.source = union_sources(&sources, &working);
                }
                if target.supersedes.is_none() {
                    target.supersedes = Some(sources[0].clone());
                }
                match vet(target, &working, today, None, None) {
                    Ok(entry) => {
                        for id in &sources {
                            if let Some(old) = working.get(id).cloned()
                                && old.status != EntryStatus::Superseded
                            {
                                let mut old = old;
                                old.status = EntryStatus::Superseded;
                                store.write_entry(&old)?;
                                working.insert(id.clone(), old);
                                outcome.superseded += 1;
                            }
                        }
                        store.write_entry(&entry)?;
                        working.insert(entry.id.clone(), entry);
                        outcome.merged += 1;
                    }
                    Err(reason) => outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason,
                    }),
                }
            }
            ConsolidateOp::Supersede { id, mut replacement } => {
                let id = id.trim().to_string();
                let Some(old) = working.get(&id).cloned() else {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "supersede target does not exist".into(),
                    });
                    continue;
                };
                if replacement.source.is_empty() {
                    replacement.source = old.source.clone();
                }
                replacement.supersedes = Some(id.clone());
                match vet(replacement, &working, today, None, None) {
                    Ok(entry) => {
                        if old.status != EntryStatus::Superseded {
                            let mut old = old;
                            old.status = EntryStatus::Superseded;
                            store.write_entry(&old)?;
                            working.insert(id.clone(), old);
                            outcome.superseded += 1;
                        }
                        store.write_entry(&entry)?;
                        working.insert(entry.id.clone(), entry);
                    }
                    Err(reason) => outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason,
                    }),
                }
            }
            ConsolidateOp::Requalify { id, scope, body } => {
                let id = id.trim().to_string();
                let Some(old) = working.get(&id).cloned() else {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify target does not exist".into(),
                    });
                    continue;
                };
                if old.status == EntryStatus::Superseded {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify a superseded entry".into(),
                    });
                    continue;
                }
                let mut entry = old;
                let mut changed = false;
                if let Some(scope) = scope {
                    let scope = scope.trim();
                    entry.scope = if scope.is_empty() {
                        None
                    } else {
                        Some(scope.to_string())
                    };
                    changed = true;
                }
                if let Some(body) = body {
                    let body = body.trim();
                    if body.is_empty() {
                        outcome.rejected.push(Rejected {
                            id: Some(id),
                            reason: "requalify with empty body".into(),
                        });
                        continue;
                    }
                    entry.body = body.to_string();
                    changed = true;
                }
                if !changed {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify with no changes".into(),
                    });
                    continue;
                }
                store.write_entry(&entry)?;
                working.insert(id, entry);
                outcome.requalified += 1;
            }
        }
    }
    Ok(outcome)
}

fn union_sources(ids: &[String], working: &HashMap<String, Entry>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if let Some(entry) = working.get(id) {
            for ref_ in &entry.source {
                if !out.contains(ref_) {
                    out.push(ref_.clone());
                }
            }
        }
    }
    out
}

/// 定期整理（睡眠学习）：全量扫描 → 模型提议 → 确定性自检 → 合入 → 重建索引。
///
/// 条目不足 2 条时直接返回空（没什么可整理的）。`sessions_dir` 给定时做**回原始证据
/// 核查**；给不出（如无会话目录）就退化为纯条目整理。
pub async fn consolidate(
    agent: &Agent,
    store: &MemoryStore,
    sessions_dir: Option<&Path>,
) -> Result<ConsolidateOutcome, CuratorError> {
    let pairs: Vec<(String, Entry)> = store
        .list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    if pairs.len() < 2 {
        return Ok(ConsolidateOutcome {
            empty: true,
            ..Default::default()
        });
    }
    let existing: HashMap<String, Entry> = pairs
        .iter()
        .map(|(_, entry)| (entry.id.clone(), entry.clone()))
        .collect();

    let evidence = match sessions_dir {
        Some(dir) => collect_evidence(&pairs, dir, MAX_EVIDENCE_CHARS),
        None => String::new(),
    };
    let prompt = build_consolidate_prompt(&pairs, &evidence);

    let raw = agent
        .complete(&prompt)
        .await
        .map_err(|err| CuratorError::Model(err.to_string()))?;
    let ops = parse_consolidate_ops(&raw)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let outcome = apply_consolidate_ops(store, ops, &existing, &today)?;
    rebuild_index(store)?;
    rebuild_core(store)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            entry_type: EntryType::Preference,
            subject: "rust-error-handling".into(),
            created_at: Some("2026-05-12".into()),
            valid_from: None,
            supersedes: None,
            confidence: Some(Confidence::High),
            scope: None,
            source: vec!["sessions/abc.jsonl#turn:2".into()],
            body: "偏好用 thiserror 定义领域错误。".into(),
        }
    }

    #[test]
    fn parse_plain_and_fenced_json() {
        let plain = r#"[{"id":"a","type":"fact","subject":"s","body":"b","source":["x"]}]"#;
        assert_eq!(parse_candidates(plain).unwrap().len(), 1);
        let fenced = format!("blah\n```json\n{plain}\n```\ntrailing");
        assert_eq!(parse_candidates(&fenced).unwrap().len(), 1);
    }

    #[test]
    fn parse_empty_array() {
        assert!(parse_candidates("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(parse_candidates("no array here").is_err());
    }

    #[test]
    fn vet_accepts_valid_candidate() {
        let entry = vet(candidate("pref-rust"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        assert_eq!(entry.id, "pref-rust");
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.source, vec!["sessions/abc.jsonl#turn:2"]);
    }

    #[test]
    fn vet_rejects_bad_slug() {
        let mut c = candidate("Bad Id!");
        c.source = vec!["x".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_missing_source_without_session_ref() {
        let mut c = candidate("pref-x");
        c.source.clear();
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_stamps_session_ref_when_source_empty() {
        let mut c = candidate("pref-x");
        c.source.clear();
        let entry = vet(
            c,
            &HashMap::new(),
            "2026-05-12",
            Some("sessions/abc.jsonl"),
            None,
        )
        .unwrap();
        assert_eq!(entry.source, vec!["sessions/abc.jsonl"]);
    }

    #[test]
    fn vet_rejects_bad_date() {
        let mut c = candidate("pref-x");
        c.created_at = Some("2026/05/12".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_dangling_supersedes() {
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_accepts_existing_supersedes() {
        let old = vet(candidate("pref-old"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        let mut existing = HashMap::new();
        existing.insert(old.id.clone(), old);
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &existing, "2026-05-12", None, None).is_ok());
    }

    #[test]
    fn vet_checks_source_file_exists() {
        let dir = std::env::temp_dir().join(format!("shirley_curator_src_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("s.jsonl"), "").unwrap();
        let mut c = candidate("pref-x");
        c.source = vec!["s.jsonl#turn:1".into()];
        assert!(vet(c.clone(), &HashMap::new(), "2026-05-12", None, Some(&dir)).is_ok());
        c.source = vec!["missing.jsonl#turn:1".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_transcript_includes_user_and_tool() {
        let messages = vec![
            Message::System {
                content: "ignore me".into(),
            },
            Message::User {
                content: "你好".into(),
            },
            Message::Assistant {
                content: Some("在".into()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("## User"));
        assert!(text.contains("你好"));
        assert!(!text.contains("ignore me"));
    }

    #[test]
    fn build_prompt_mentions_existing_and_boundary() {
        let existing = vec![(
            "preferences/a.md".into(),
            vet(candidate("pref-a"), &HashMap::new(), "2026-05-12", None, None).unwrap(),
        )];
        let prompt = build_prompt("## User\nhi\n", &existing, "sessions/x.jsonl");
        assert!(prompt.contains("部分证据"));
        assert!(prompt.contains("pref-a"));
        assert!(prompt.contains("sessions/x.jsonl#turn:N"));
    }

    // ---- V2-B：定期整理（consolidate）------------------------------------

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_curator_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write_entry(store: &MemoryStore, id: &str, status: EntryStatus) -> Entry {
        let mut entry = vet(candidate(id), &HashMap::new(), "2026-05-10", None, None).unwrap();
        entry.status = status;
        store.write_entry(&entry).unwrap();
        entry
    }

    #[test]
    fn parse_consolidate_ops_plain_and_fenced() {
        let plain = r#"[{"op":"requalify","id":"a","scope":"x"}]"#;
        assert_eq!(parse_consolidate_ops(plain).unwrap().len(), 1);
        let fenced = format!("blah\n```json\n{plain}\n```\ntail");
        assert_eq!(parse_consolidate_ops(&fenced).unwrap().len(), 1);
    }

    #[test]
    fn apply_merge_supersedes_sources_and_writes_target() {
        let root = tmpdir("merge");
        let store = MemoryStore::new(&root);
        let a = write_entry(&store, "a", EntryStatus::Active);
        let b = write_entry(&store, "b", EntryStatus::Active);
        let existing: HashMap<String, Entry> =
            [("a".to_string(), a), ("b".to_string(), b)].into_iter().collect();

        let mut target = candidate("ab-merged");
        target.source.clear(); // 触发从 sources 继承证据
        let op = ConsolidateOp::Merge {
            target,
            sources: vec!["a".into(), "b".into()],
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.merged, 1);
        assert_eq!(out.superseded, 2);
        assert!(out.rejected.is_empty());

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["a"].status, EntryStatus::Superseded);
        assert_eq!(by_id["b"].status, EntryStatus::Superseded);
        assert_eq!(by_id["ab-merged"].status, EntryStatus::Active);
        assert!(!by_id["ab-merged"].source.is_empty(), "证据应从旧条目继承");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_supersede_marks_old_and_links_new() {
        let root = tmpdir("supersede");
        let store = MemoryStore::new(&root);
        let old = write_entry(&store, "old", EntryStatus::Active);
        let existing: HashMap<String, Entry> = [("old".to_string(), old)].into_iter().collect();

        let mut replacement = candidate("new");
        replacement.source.clear();
        let op = ConsolidateOp::Supersede {
            id: "old".into(),
            replacement,
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.superseded, 1);
        assert!(out.rejected.is_empty());

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["old"].status, EntryStatus::Superseded);
        assert_eq!(by_id["new"].supersedes.as_deref(), Some("old"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_requalify_updates_scope_without_new_entry() {
        let root = tmpdir("requalify");
        let store = MemoryStore::new(&root);
        let entry = write_entry(&store, "topic", EntryStatus::Active);
        let existing: HashMap<String, Entry> = [("topic".to_string(), entry)].into_iter().collect();

        let op = ConsolidateOp::Requalify {
            id: "topic".into(),
            scope: Some("仅在 Linux".into()),
            body: None,
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.requalified, 1);

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["topic"].scope.as_deref(), Some("仅在 Linux"));
        assert_eq!(by_id.len(), 1, "requalify 不应新建条目");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_rejects_merge_with_unknown_source() {
        let root = tmpdir("reject");
        let store = MemoryStore::new(&root);
        let existing: HashMap<String, Entry> = HashMap::new();
        let op = ConsolidateOp::Merge {
            target: candidate("m"),
            sources: vec!["ghost".into()],
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.merged, 0);
        assert_eq!(out.rejected.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_evidence_reads_turn_line_and_flags_missing() {
        let dir = tmpdir("evidence");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.jsonl"), "line1\nline2-topic\nline3\n").unwrap();
        let mut e = vet(candidate("e"), &HashMap::new(), "2026-05-10", None, None).unwrap();
        e.source = vec!["s.jsonl#turn:2".into(), "missing.jsonl#turn:1".into()];
        let text = collect_evidence(&[("e.md".into(), e)], &dir, MAX_EVIDENCE_CHARS);
        assert!(text.contains("line2-topic"));
        assert!(text.contains("[missing evidence]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_consolidate_prompt_lists_entries_and_evidence() {
        let entry = vet(candidate("pref-a"), &HashMap::new(), "2026-05-10", None, None).unwrap();
        let prompt = build_consolidate_prompt(&[("preferences/a.md".into(), entry)], "EVIDENCE");
        assert!(prompt.contains("pref-a"));
        assert!(prompt.contains("EVIDENCE"));
        assert!(prompt.contains("永不删除历史"));
    }


    // ---- V2.5：core.md 确定性重建 ----------------------------------------

    fn active_entry(id: &str, entry_type: EntryType, date: &str) -> Entry {
        let mut entry = vet(candidate(id), &HashMap::new(), date, None, None).unwrap();
        entry.entry_type = entry_type;
        entry.created_at = date.to_string();
        entry
    }

    #[test]
    fn rebuild_core_projects_active_entries_in_type_order() {
        let root = tmpdir("core_build");
        let store = MemoryStore::new(&root);
        store.write_entry(&active_entry("evt", EntryType::Event, "2026-05-12")).unwrap();
        store.write_entry(&active_entry("fact", EntryType::Fact, "2026-05-11")).unwrap();
        store.write_entry(&active_entry("pref", EntryType::Preference, "2026-05-10")).unwrap();

        rebuild_core(&store).unwrap();
        let core = store.read_core().expect("core 应已生成");
        // 类型顺序：preference → fact → event（画像 / 偏好在前）。
        let p = core.find("[preference]").expect("应含偏好");
        let f = core.find("[fact]").expect("应含事实");
        let e = core.find("[event]").expect("应含事件");
        assert!(p < f && f < e, "core 类型顺序应为 preference < fact < event");
        // 每条一行，含摘要（正文首行）。
        assert!(core.contains("偏好用 thiserror 定义领域错误。"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_core_skips_non_active_entries() {
        let root = tmpdir("core_skip");
        let store = MemoryStore::new(&root);
        store.write_entry(&active_entry("keep", EntryType::Fact, "2026-05-10")).unwrap();
        let mut gone = active_entry("gone", EntryType::Fact, "2026-05-10");
        gone.status = EntryStatus::Superseded;
        store.write_entry(&gone).unwrap();

        rebuild_core(&store).unwrap();
        let core = store.read_core().unwrap();
        assert!(core.contains("[fact]"));
        // superseded 条目不进常驻层（唯一一行应来自 keep）。
        assert_eq!(core.matches("- [").count(), 1, "只应有 1 条活跃条目进 core");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_core_removes_stale_core_when_no_active_entries() {
        let root = tmpdir("core_clear");
        let store = MemoryStore::new(&root);
        store.write_core("用户是 Sean。").unwrap();
        assert!(store.read_core().is_some());

        // 无活跃条目 → 应删除旧 core.md（避免残留过时画像被继续注入）。
        rebuild_core(&store).unwrap();
        assert!(store.read_core().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
